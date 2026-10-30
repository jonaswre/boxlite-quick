#!/usr/bin/env bash
# PROTOTYPE, throwaway. Non-interactive checks for jonaswre/boxlite#5.
# Run from this directory in a normal host shell (not inside an agent sandbox),
# with ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN exported.
set -u
cd "$(dirname "$0")"
S=./target/debug/box-claude-spike
WS="$PWD/PROTOTYPE-workspace"
mkdir -p "$WS" && (cd "$WS" && git init -q 2>/dev/null; echo "hello from host" > README.md)
x() { "$S" exec "$WS" -- sh -c "$1"; }
hr() { printf '\n=== %s\n' "$*"; }

hr "1. cold boot + setup (timings on stderr)"
"$S" up "$WS" || { echo "up failed; if the error mentions bwrap/userns, retry with SPIKE_NO_JAILER=1"; exit 1; }

hr "2. warm reattach (expect 'box found (reattach)' and 'setup: already installed')"
"$S" up "$WS"

hr "3. key never enters the VM (expect the placeholder, not your key)"
x 'printenv ANTHROPIC_AUTH_TOKEN; env | grep -c "^ANTHROPIC_AUTH_TOKEN=<BOXLITE_SECRET:" '

hr "4. placeholder is swapped on the wire (expect HTTP 200 from CLIProxyAPI)"
x 'curl -sS -o /dev/null -w "%{http_code}\n" -m 20 -H "Authorization: Bearer $ANTHROPIC_AUTH_TOKEN" "$ANTHROPIC_BASE_URL/v1/models"'

hr "5. egress: registry allowed, everything else blocked"
x 'curl -sS -o /dev/null -w "npm %{http_code}\n" -m 15 https://registry.npmjs.org/ || echo "npm FAILED"'
for u in https://example.com/ https://api.github.com/ http://example.com/; do
  x "curl -sS -o /dev/null -m 10 $u 2>/dev/null && echo '$u REACHABLE (bad)' || echo '$u blocked (good)'"
done

hr "6. Workspace edits land on the host with your ownership"
x "echo from-box > '$WS/from-box.txt'; mkdir -p '$WS/box-dir'; touch '$WS/box-dir/f'; id"
stat -c '%n %U:%G %a' "$WS/from-box.txt" "$WS/box-dir" "$WS/box-dir/f"
echo "host user: $(id -un):$(id -gn)"

hr "7. state survives stop + reattach"
x 'echo marker > /root/state-marker'
"$S" stop "$WS"
"$S" up "$WS"
x 'cat /root/state-marker && claude --version'

hr "done. Next: '$S run $WS' for the interactive Claude Code session."
