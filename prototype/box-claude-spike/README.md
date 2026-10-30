# PROTOTYPE: box-claude-spike (throwaway, never merged)

Answers [Can a Box run Claude Code end to end against CLIProxyAPI over Tailscale?](https://github.com/jonaswre/boxlite/issues/5).
This is not boxq; it is a single-file spike on `boxlite = "=0.11.0"`.

What it does on `run`:

1. Opens a BoxLite runtime at `~/.boxq-spike-PROTOTYPE-wipe-me` (delete this directory to wipe everything).
2. `get_or_create`s a detached, persistent Box (`auto_delete: Some(0)`, main command `sleep infinity`) named after a hash of the Workspace path. Image: `node:22-bookworm` (override with `SPIKE_IMAGE`).
3. Binds the Workspace read-write at the same path.
4. Passes `ANTHROPIC_BASE_URL` through and sets `ANTHROPIC_AUTH_TOKEN=<BOXLITE_SECRET:anthropic>`. The real key goes only into `BoxOptions.secrets`, scoped to the base-URL host.
5. Egress `allow_net` = base-URL host + its tailnet IP + `registry.npmjs.org`. Nothing else.
6. Setup runs once (marker `/opt/boxq-setup-done`): `npm i -g @anthropic-ai/claude-code@2.1.312`, then seeds `/root/.claude.json` and `/root/.claude/settings.json` to skip the onboarding, trust and bypass prompts.
7. `exec`s `claude --dangerously-skip-permissions` on a TTY as root, with `IS_SANDBOX=1`.

Every step prints a `[spike +Ns]` timing to stderr.

## Run it

Use a normal host shell. An agent sandbox with seccomp or no-new-privs won't work: image unpacking needs to create setgid files, and the jailer needs user namespaces.

```sh
cd prototype/box-claude-spike
cargo build
export ANTHROPIC_BASE_URL=https://cliproxyapi.<tailnet>.ts.net ANTHROPIC_AUTH_TOKEN=...
./observe.sh                                        # non-interactive checks 1-7
./target/debug/box-claude-spike run PROTOTYPE-workspace   # the interactive session
```

If boot fails with a bwrap or user-namespace error, this host has `kernel.apparmor_restrict_unprivileged_userns=1`. Either rerun with `SPIKE_NO_JAILER=1`, or run `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`. Note which one you used.

Other subcommands: `exec <path> -- cmd...`, `stop <path>`, `rm <path>`, `ls`.

`src/terminal.rs` is copied verbatim from boxlite-cli 0.11.0 (Apache-2.0).
