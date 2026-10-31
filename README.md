# boxq

boxq runs Claude Code against a Workspace, a directory on your host, inside an isolated Sandbox. It is an open alternative to Docker's sbx, built on [BoxLite](https://github.com/boxlite-ai/boxlite).

`boxq run` in any directory (a git worktree too) creates or reattaches to that directory's Sandbox and starts Claude Code in it, with every permission prompt off, since the Sandbox is the boundary. Each Workspace gets one persistent Sandbox:

- The Workspace is mounted read-write at the same path. In a git worktree, the repo's common `.git` is mounted at its own path too, so git works unchanged.
- Your Claude skills, agents, commands, settings, `CLAUDE.md` and MCP servers are copied in on every run. Credentials never are.
- Claude Code authenticates through a base URL and API key, for example CLIProxyAPI. Over HTTPS the key never enters the Sandbox: the guest holds a placeholder, which BoxLite swaps for the key in requests to the base URL's host.
- Outbound traffic is deny-by-default: an Egress Policy says what the Sandbox may reach.

This README is for boxq's user and contributors. `GLOSSARY.md` defines the terms (Sandbox, Workspace, Egress Policy, …), and `docs/adr/` records the decisions behind them.

## Host prerequisites

boxq runs only on Linux x86_64 with KVM. These commands are for Ubuntu 24.04, the one host it is tested on. boxq doesn't check any of this up front: when one is missing, BoxLite's report says which, and [Troubleshooting](#troubleshooting) maps it to its fix.

**KVM.** `/dev/kvm` has to exist, and you need read-write access to it:

```sh
ls -l /dev/kvm                  # missing: enable VT-x/AMD-V in the firmware, then `sudo modprobe kvm_intel` (or kvm_amd)
sudo usermod -aG kvm "$USER"    # then log in again
```

**bubblewrap with user namespaces.** BoxLite jails each Sandbox's process with `bwrap`, which has to create user namespaces. Ubuntu 24.04 refuses them (`kernel.apparmor_restrict_unprivileged_userns=1`) unless an AppArmor profile allows them for `/usr/bin/bwrap`. Install bubblewrap and this profile, which keeps the restriction in place for everything else:

```sh
sudo apt install bubblewrap
sudo tee /etc/apparmor.d/boxlite-bwrap >/dev/null <<'EOF'
# BoxLite 0.11.0 profile adapted for root-owned Ubuntu /usr/bin/bwrap.
abi <abi/4.0>,

include <tunables/global>

profile boxlite_bwrap /usr/bin/bwrap flags=(attach_disconnected,mediate_deleted) {
  allow capability,
  allow file rwlkm /{**,},
  allow network,
  allow unix,
  allow ptrace,
  allow signal,
  allow mqueue,
  allow io_uring,
  allow userns,
  allow mount,
  allow umount,
  allow pivot_root,
  allow dbus,

  # Stacked child profile for no-new-privs (bwrap uses PR_SET_NO_NEW_PRIVS)
  allow pix /** -> &boxlite_bwrap//&boxlite_unpriv_bwrap,
}

# Child profile strips capabilities within the user namespace
profile boxlite_unpriv_bwrap flags=(attach_disconnected,mediate_deleted) {
  allow file rwlkm /{**,},
  allow network,
  allow unix,
  allow ptrace,
  allow signal,
  allow mqueue,
  allow io_uring,
  allow userns,
  allow mount,
  allow umount,
  allow pivot_root,
  allow dbus,

  allow pix /** -> &boxlite_unpriv_bwrap,

  audit deny capability,
}
EOF
sudo apparmor_parser -r /etc/apparmor.d/boxlite-bwrap
```

The quick and less secure alternative lifts the restriction for every program, as CI does: `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`.

**Setuid/setgid files and a mountable `/proc`.** Unpacking an image creates setuid/setgid files, and bwrap mounts a fresh `/proc`. A normal login shell can do both. Some sandboxes can't: systemd's `RestrictSUIDSGID` and `ProtectKernelTunables` block them, as do some agents' own sandboxes. If boxq runs from a systemd unit, set:

```ini
[Service]
RestrictSUIDSGID=no
ProtectKernelTunables=no
```

**A writable cgroup (optional).** BoxLite puts each Sandbox in a cgroup under your systemd user manager (`/sys/fs/cgroup/user.slice/user-<uid>.slice/user@<uid>.service/boxlite`) when it can write there. Where it can't, as over some SSH sessions, in CI or in a system service, it runs the Sandbox without cgroup limits and says nothing. boxq then kills the Sandbox's leftover processes itself on `stop` and `rm`.

**Other distros** are untested. BoxLite needs the same things everywhere: an accessible `/dev/kvm`, a `bwrap` that can create user namespaces (check with `bwrap --unshare-user --ro-bind / / true`), and permission to create setuid/setgid files and mount `/proc`. Without a system `bwrap`, BoxLite falls back to the one it bundles.

## Building

You need rustup, which installs the Rust release `rust-toolchain.toml` pins, and `protoc` 3.12 or later, which BoxLite's build uses:

```sh
sudo apt install protobuf-compiler
cargo build --release           # the binary is target/release/boxq
```

`cargo test` runs the tests that need no KVM. The ones that boot a real Sandbox are ignored by default. Run them with `cargo test -- --ignored`, on a host with the prerequisites above, as CI's `real-box` job does. CI's `test` job also fails on `cargo fmt --check` and `cargo clippy --all-targets --locked -- -D warnings`.

## Commands

Each command takes a Workspace path, which defaults to the current directory. A Workspace is identified by its canonical absolute path.

| Command | What it does |
| --- | --- |
| `boxq run [path]` | Creates the Workspace's Sandbox or reattaches to it, runs any Setup Hook that hasn't succeeded, copies your Claude config in, and starts Claude Code there as root in the Workspace. |
| `boxq shell [path]` | The same, but opens root's login shell instead of Claude Code. It opens even when a Setup Hook failed, so you can debug it. |
| `boxq ls` | Lists every Sandbox by Workspace path, running or stopped. |
| `boxq stop [path]` | Stops the Workspace's Sandbox and keeps its state. |
| `boxq rm [path]` | Removes the Workspace's Sandbox and all its state, and forgets its Egress Approval. |

The Sandbox keeps running after a `run` or `shell` session ends, so the next one starts fast. Several Sandboxes, and `shell` next to `run` in the same one, can run at once. When two boxq commands both need BoxLite's runtime, the second prints `Waiting for another boxq command to finish...` for a few seconds at most.

## The Base Image

Unless the config sets `image`, a new Sandbox is created from boxq's Base Image, `ghcr.io/jonaswre/boxq`, pinned by digest in `src/spec.rs`. Because of the digest, a copy already pulled is used with no registry round trip, and pushing to a tag can't change what a new Sandbox gets. The first `boxq run` on a host pulls the image, which takes a few minutes.

The image is `node:22-bookworm` with Claude Code preinstalled at a pinned release, plus git, gh, ripgrep, fd, jq, python3, uv, unzip, less, procps and vim-tiny. It holds no boxq config. Language toolchains go in Setup Hooks.

boxq never installs Claude Code itself. Any image you configure must have `claude` on its PATH. If it doesn't, `boxq run` fails and says so; to switch the image, `boxq rm` the Sandbox and run again.

### Building and bumping it

`image/Dockerfile` sets the Claude Code release with `ARG CLAUDE_CODE_VERSION`. The `Base Image` workflow builds `linux/amd64` on `main` when `image/` changes, or on manual dispatch. It boots a Sandbox from that exact build and checks that Claude Code starts with no prompts. Only if that passes does it tag the build `ghcr.io/jonaswre/boxq:<claude-version>-<short sha>`. The job summary then shows the `tag@digest` ref. Bumping the image is a manual step: paste that ref into `BASE_IMAGE` in `src/spec.rs`.

A Sandbox keeps the image it was created with. After a bump, `boxq rm` a Sandbox to recreate it from the new image. That discards the Sandbox's state.

## Configuration

boxq reads three TOML files. Each is optional.

**The Global Config**, `~/.config/boxq/config.toml` (`$XDG_CONFIG_HOME/boxq/config.toml`), holds your own settings for every Sandbox:

```toml
base_url = "https://cliproxy.example.ts.net"   # https only, with no port and no credentials
image = "ghcr.io/you/agent-image:1"            # instead of the Base Image; needs `claude` on its PATH
setup = "apt-get update && apt-get install -y ripgrep"   # the Global Setup Hook

[resources]                                    # see Resources below
cpus = 8
memory = "16GiB"
disk = "64GiB"

[egress]
builtins = true                                # the built-in entries below
allow = ["registry.example.com", "*.example.org", "100.64.0.7", "10.0.0.0/8"]
allow_host_loopback = false                    # the host's 127.0.0.1, as host.boxlite.internal
```

**The credentials file**, `~/.config/boxq/credentials`, holds the API key. boxq refuses it unless only you can read it (`chmod 600`):

```toml
api_key = "sk-…"
```

**The Workspace Config**, `.boxq.toml` in the Workspace, travels with the repo, usually committed. boxq treats it as untrusted, since whoever wrote the repo wrote it (ADR 0001):

- It may set `image` and `[resources]`, which override the Global Config's, and `setup`, which runs after the Global Config's; see [Setup Hooks](#setup-hooks).
- Its `[egress] allow` entries apply only with an Egress Approval; see below.
- It may not set `base_url`, `egress.builtins` or `egress.allow_host_loopback`. boxq refuses a file that does.

**Environment variables:**

| Variable | Effect |
| --- | --- |
| `BOXQ_BASE_URL` | Overrides `base_url`. |
| `BOXQ_API_KEY` | Overrides the credentials file's `api_key`. |
| `BOXQ_HOME` | Where boxq keeps its Sandboxes and BoxLite's image cache. Defaults to `$XDG_DATA_HOME/boxq`, else `~/.local/share/boxq`. |
| `BOXQ_LOG` | Which of BoxLite's log events go to stderr, as a `tracing` `EnvFilter` (`warn`, `boxlite=debug`, `off`). Defaults to `error`, which prints BoxLite's full report on a host failure. |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME` | Move the config files and the Egress Approvals, as usual. |
| `CLAUDE_CONFIG_DIR` | Where the host's Claude config is read from, as for Claude Code itself. |

boxq never reads the host's `ANTHROPIC_*` variables, so a Sandbox never picks up a key meant for something else.

### Resources

A Sandbox's CPUs, memory and disk come from `[resources]` in the Global Config or the Workspace Config; the Workspace Config wins field by field.

```toml
[resources]
cpus = 8          # 1–16
memory = "16GiB"  # whole MiB or GiB, at most the host's total RAM
disk = "64GiB"    # whole MiB or GiB, rounded up to a whole GiB
```

Anything neither file sets gets boxq's default: **4 vCPUs, 8 GiB memory, 32 GiB disk**.

- The disk is sparse, so it uses host space only as the Sandbox writes to it. It is sized once, when the Sandbox is created, and never grows.
- Memory isn't handed back to the host while the Sandbox runs; `boxq stop` frees it.
- Resources are fixed when the Sandbox is created. After changing them in a config, boxq prints the `boxq rm` then `boxq run` command that applies them (this discards the Sandbox's state). A change to boxq's own defaults doesn't affect existing Sandboxes. Sandboxes created before boxq had defaults keep 1 vCPU and 1 GiB until you recreate them.
- A config that sets `cpus` outside 1–16 or `memory` above the host's RAM is refused when it loads, with an error naming the file.

### What a Sandbox keeps from its creation

A Sandbox keeps what it was created from: its image, resources, base URL and API key. A Setup Hook that has succeeded is never run again either. When one of these changes in the config, boxq doesn't rebuild the Sandbox on its own, since that would discard its state. It says which changed and prints:

```
run `boxq rm <path>` and then `boxq run <path>`, which discards the Sandbox's state
```

The same goes for its mounts: a Sandbox created before its Workspace became a git worktree, or before the repo moved, keeps the mounts it had, with no warning. The Egress Policy and your Claude config are the exceptions: they are applied again on every `run` and `shell`.

### Setup Hooks

The Global Config's `setup` and the Workspace Config's `setup` both run, the Global one first. Each runs once, when the Sandbox is created, before any session. It runs as root in the Workspace, through `sh -c`, under the normal Egress Policy. Write hooks that are safe to re-run:

- A hook that fails or is interrupted runs again on the next `run` or `shell`, from the current config.
- To re-run a hook that succeeded, run it by hand in `boxq shell`, or use `boxq rm` then `boxq run`.

### Claude config

Every `run` copies a curated part of the host's Claude config into the Sandbox's `/root`:

- `skills/`, `agents/`, `commands/` and `CLAUDE.md` from `~/.claude` (or `CLAUDE_CONFIG_DIR`);
- `settings.json`, without its `env`, credential helpers, `hooks`, `statusLine` and plugins, which hold secrets or run host programs;
- from `~/.claude.json`, the user-scope MCP servers and the Workspace's local-scope ones.

The host's copy wins, and nothing is deleted, so something removed on the host stays in the Sandbox until `boxq rm`. Credentials, logins and history are never copied.

MCP servers run inside the Sandbox. Two things to know:

- **Their API hosts are not allowed automatically.** Add each MCP server's API host to the Global Config's `allow` by hand. boxq doesn't work them out from the MCP config.
- **Their secrets enter the Sandbox.** MCP secrets are copied as-is into the guest's `~/.claude.json` (mode `0600`). Unlike the API key, they don't use a placeholder, so the Egress Policy is what limits where they can go.

## Egress Policy

A Sandbox reaches only what its Egress Policy allows. It always allows:

- `api.anthropic.com`;
- the base URL's host and, where it resolves, its IPv4 address.

Unless the Global Config sets `builtins = false`, it also allows: `github.com`, `*.github.com`, `*.githubusercontent.com`, `registry.npmjs.org`, `pypi.org`, `files.pythonhosted.org`, `crates.io`, `*.crates.io`, apt's `deb.debian.org` and `security.debian.org`, and rustup's `static.rust-lang.org` and `sh.rustup.rs`.

To these it adds the Global Config's `allow`, and the Workspace Config's `allow` once approved. Each entry is a hostname, a `*.` wildcard, an IPv4 address or an IPv4 CIDR. Ports, schemes and IPv6 are refused, because BoxLite would silently drop them. A hostname that resolves into the tailnet range (`100.64.0.0/10`) gets its IP allowed too, since BoxLite refuses such a name by hostname alone.

The host's loopback is blocked unless the Global Config sets `allow_host_loopback = true`. The Sandbox then reaches the host's `127.0.0.1` as `host.boxlite.internal`. An `allow` entry that would open it any other way is an error.

### Egress Approvals

A repo's `.boxq.toml` could open an exfiltration path, so its `allow` entries apply only with your Egress Approval. The first time a Workspace has them, and whenever they change, `run` and `shell` list them and ask `Allow it? [y/N]`. Without a terminal to ask on, boxq runs without them and says so. Approvals live in `~/.local/state/boxq/approvals.toml` (`$XDG_STATE_HOME/boxq`), and `boxq rm` forgets the Workspace's one.

## Troubleshooting

When BoxLite can't start a Sandbox, boxq's error names the step that failed and the Workspace. BoxLite's own error says only a short sentence, so its full report is printed just before it, starting `ERROR boxlite::…`. The report has the paths, the failing tool's stderr and the fix commands. To see more of BoxLite's log, set `BOXQ_LOG=boxlite=debug`.

| Message | Cause | Fix |
| --- | --- | --- |
| `KVM is not available on this host.` | `/dev/kvm` doesn't exist. | Enable VT-x/AMD-V in the firmware and load `kvm_intel` or `kvm_amd`. On a cloud VM, enable nested virtualization. |
| `KVM is available but access is denied for the current user.` | You can't open `/dev/kvm`. | `sudo usermod -aG kvm "$USER"`, then log in again. |
| `KVM is present but cannot run a virtual machine on this host.` | `/dev/kvm` exists but can't run guest code. | Enable nested virtualization, or use a host with hardware virtualization. |
| `The sandbox could not be initialized on this host.` | bwrap can't create user namespaces; the report shows `bwrap --unshare-user failed` and its cause. | Install bubblewrap and the AppArmor profile from [Host prerequisites](#host-prerequisites). |
| `The VM failed to start.`, after a `Box crash details` report whose stderr shows bwrap failing to mount `/proc` | `/proc` isn't mountable, as under `ProtectKernelTunables`. | Run boxq outside that sandbox, or set `ProtectKernelTunables=no`. |
| `storage error: Failed to create file …: Operation not permitted` while unpacking an image | Setuid/setgid files can't be created, as under `RestrictSUIDSGID`. There is no fuller report for this one. | Run boxq outside that sandbox, or set `RestrictSUIDSGID=no`. |

The KVM checks run whenever boxq opens BoxLite's runtime, so without KVM `ls`, `stop` and `rm` fail too. The others fail only when a Sandbox is created or started. After a failed start, BoxLite's report says where it kept the Box's diagnostic files and suggests `boxlite rm`; use `boxq rm` instead, or just `boxq run` again once the host is fixed.
