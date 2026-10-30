# PROTOTYPE: box-claude-spike (throwaway, never merged)

Answers [Can a Box run Claude Code end to end against CLIProxyAPI over Tailscale?](https://github.com/jonaswre/boxlite/issues/5).
This is not boxq; it is a single-file spike on `boxlite = "=0.11.0"`.

What it does on `run`:

1. Opens a BoxLite runtime at `~/.boxq-spike-PROTOTYPE-wipe-me` (delete this directory to wipe everything).
2. `get_or_create`s a detached, persistent Box (`auto_delete: Some(0)`, main command `sleep infinity`) named after a hash of the Workspace path. Image: `node:22-bookworm` (override with `SPIKE_IMAGE`).
3. Binds the Workspace read-write at the same path.
4. Passes `ANTHROPIC_BASE_URL` through and sets `ANTHROPIC_AUTH_TOKEN=<BOXLITE_SECRET:anthropic>`. The real key goes only into `BoxOptions.secrets`, scoped to the base-URL host.
5. Egress `allow_net` = base-URL host + its tailnet IP + `registry.npmjs.org`. Nothing else.
6. Installs Claude once (marker `/opt/boxq-setup-done`): `npm i -g @anthropic-ai/claude-code@2.1.312`. On every attachment, atomically merges onboarding/trust and guest permission settings, including `disableAutoMode: "disable"`, while preserving unrelated configuration.
7. `exec`s `claude --dangerously-skip-permissions` on a TTY as root, with `IS_SANDBOX=1`.

Every step prints a `[spike +Ns]` timing to stderr.

## Run it

Image extraction must be allowed to preserve setuid/setgid mode bits. For a systemd-launched host process, set `RestrictSUIDSGID=false` and retain `NoNewPrivileges=true`. Also set `ProtectKernelTunables=false`: its masked proc mounts prevent bubblewrap from mounting the guest's private `/proc`. Keep the service non-root with an empty capability bounding set. Restart the parent service and start new agent processes after these changes; existing processes retain inherited restrictions.

```sh
cd prototype/box-claude-spike
cargo build
export ANTHROPIC_BASE_URL=https://cliproxyapi.<tailnet>.ts.net ANTHROPIC_AUTH_TOKEN=...
./observe.sh                                        # non-interactive checks 1-7
./target/debug/box-claude-spike run PROTOTYPE-workspace   # the interactive session
```

On Ubuntu 24.04, install system `bubblewrap` and its specific AppArmor userns profile. The T3 hosts use `/etc/apparmor.d/boxlite-bwrap`, adapted from the pinned BoxLite SDK's generated profile for `/usr/bin/bwrap`. Keep `kernel.apparmor_restrict_unprivileged_userns=1` and the BoxLite jailer enabled. Verify as the unprivileged service user: `bwrap --ro-bind / / --unshare-user --unshare-pid --proc /proc true`. Run this under the service's effective hardening too; a normal terminal alone does not verify service compatibility.

Other subcommands: `exec <path> -- cmd...`, `stop <path>`, `rm <path>`, `ls`.

`src/terminal.rs` is copied verbatim from boxlite-cli 0.11.0 (Apache-2.0).
