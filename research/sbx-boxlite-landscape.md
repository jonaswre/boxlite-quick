# Docker Sandboxes (sbx) and BoxLite: landscape briefing (2026-10-30)

Background for the [boxq wayfinder map](https://github.com/jonaswre/boxlite/issues/1). Throwaway research branch; never merged. Claims marked **[binary inspection]** come from inspecting downloaded release artifacts, not docs.

## 1. Docker Sandboxes (`sbx`)

**Name, status and version**
- The CLI is a standalone binary, **`sbx`**. The old `docker sandbox` plugin was removed in Docker Desktop 4.80.0 in favour of `sbx`. https://docs.docker.com/retired/
- No Docker Desktop or Engine needed, but a Docker login is required (`sbx login`, browser OAuth). https://docs.docker.com/ai/sandboxes/install/, https://docs.docker.com/ai/sandboxes/faq/
- Latest release v0.50.1 (2026-10-28). Releases are frequent, and v0.48.0 refactored the CLI with breaking changes. https://github.com/docker/sbx-releases/releases
- A host daemon, `sandboxd`, runs behind the CLI.

**Subcommands** (from `data/sbx_cli/sbx.yaml` in https://github.com/docker/docs)
- Top level: `attach`, `cp`, `create`, `daemon`, `diagnose`, `exec`, `login`, `logout`, `ls`, `mcp`, `move`, `policy`, `ports`, `prune`, `reset`, `rm`, `run`, `secret`, `settings`, `stop`, `template`, `ttl`, `tui`, `version`, `volume`.
- Experimental: `env`, `kit`, `setup`, `skills`.
- `create` has a subcommand per agent: `claude`, `codex`, `cursor`, `devin`, `docker-agent`, `gemini`, `opencode`, `shell`.

**Isolation**
- Each sandbox is a "lightweight microVM with its own Linux kernel". The agent runs as a non-root user with sudo. https://docs.docker.com/ai/sandboxes/security/isolation
- Linux uses KVM (the user must be in the `kvm` group); Windows uses the Windows Hypervisor Platform.
- VMM:
  - Earlier versions shipped `containerd/nerdbox` with libkrun. https://github.com/docker/sbx-releases/issues/16
  - **[binary inspection]** v0.50.1 instead ships a proprietary Rust VMM, `libsailor`, built on `kvm-ioctls` and `vm-memory`. On macOS it uses Hypervisor.framework.

**Hosts:** macOS 14+ (Apple silicon), Windows 11 x86-64, Ubuntu 24.04+ (x86-64/arm64). Ubuntu derivatives are not supported. https://docs.docker.com/ai/sandboxes/install/

**Workspace**
- Direct mode is the default: virtiofs, read-write, at **the same absolute path** as on the host, with no sync step.
- Clone mode: the repo is mounted read-only and the agent works on a private clone. It doesn't work from a non-main worktree.
- Mountless mode: no host directory at all.
- https://docs.docker.com/ai/sandboxes/architecture/

**Network**
- All TCP goes through a host proxy: HTTP/HTTPS through a forward proxy with TLS interception, other TCP transparently.
- **Deny-by-default**, managed with `sbx policy allow`. UDP is off and ICMP is blocked.
- Sandbox-to-sandbox traffic and access to the host Docker daemon are blocked.
- https://docs.docker.com/ai/sandboxes/security/defaults

**Credentials**
- `sbx secret set` stores secrets in the OS keychain. On headless Linux it falls back to a file.
- The agent sees a sentinel value; the host proxy substitutes the real one in HTTP **headers only**.
- Claude Code uses `/login` inside the sandbox.
- https://docs.docker.com/ai/sandboxes/configuration/credentials/

**Agents**
- About ten are supported. Claude Code uses `docker/sandbox-templates:claude-code` and runs in bypass-permissions mode by default.
- Host `~/.claude` is **not** carried in.
- Environments are defined as "Kits": YAML descriptors published as OCI images.
- Stdio MCP servers run **on the host**.
- https://docs.docker.com/ai/sandboxes/agents/claude-code, https://docs.docker.com/ai/sandboxes/customize

**Docker-in-Docker:** yes, each sandbox gets its own Engine.

**Lifecycle**
- `sbx run` in the same path reattaches. Stopping keeps state; `rm` deletes.
- Also available: `--rm`, `-d`, named sandboxes.

**Licensing:** proprietary ("All rights reserved"). Free for local use. Telemetry is on by default.

**Notable issues** (https://github.com/docker/sbx-releases/issues)
- Virtiofs:
  - silent data loss / sparse files (#526)
  - `link()`/`fsync` breaking `git clone` (#629)
  - case folding (#666)
  - empty workspace on Windows (#449)
- Sandbox removed after sleep (#413).
- Very slow starts (#621, #681).
- Proxy:
  - HTTPS empty reply (#652)
  - SSE buffering (#480)
- Headless `secret import` broke (#471).

## 2. BoxLite (https://github.com/boxlite-ai/boxlite)

**Project**
- Apache-2.0, about 2.6k stars, created 2025-12. Latest release v0.11.0 (2026-10-30), with roughly weekly releases before it.
- The core is in Rust. There's also a large TypeScript cloud/dashboard, and a hosted cloud.

**Implementation**
- An embeddable library: no daemon, no root.
- One VM per box, run through a per-box shim, with an OCI container inside the VM.
- Jailer: seccomp + bubblewrap on Linux.
- Currently uses forks of **libkrun/libkrunfw**. A native VMM (KVM/HVF, later WHP) is replacing it; libkrun stays until milestone M5. Memory snapshots, hot-plug and GPU are planned for M6–M9. See `docs/contributing/architecture/vmm/README.md`.
- Networking: gvproxy (gVisor netstack) by default; libslirp is the alternative.

**Hosts:** macOS Apple Silicon (12+), Linux x86_64/ARM64 with `/dev/kvm`, Windows via WSL2.

**Interfaces**
- SDKs for Python, Node, Go (CGO), Rust (the `boxlite` crate) and C.
- CLI: `boxlite run|exec|create|list|rm|start|stop|restart|pull|images|inspect|cp|info|logs|stats|network|ssh|agent|serve|volume|...`
- A REST server (`boxlite serve`).

**Images:** any OCI image, with pull/cache and private registries.

**Filesystem**
- Host directories are shared over virtiofs, mounted with Docker-style `-v HOST:BOX[:ro|rw]`.
- Each box gets a QCOW2 copy-on-write disk.
- Nothing documents same-path mounting; you choose the box path yourself.

**Network**
- Outbound is **open by default**. `allow_net` restricts it to hostnames, wildcards, IPs or CIDRs, and can be updated live.
- Inbound is off by default; port publishing and tunnels are available.
- Caveat: with an empty `allow_net`, host loopback is reachable.
- `docs/guides/networking.md`

**Secrets**
- Configured as `{name, value, hosts, placeholder}`.
- The guest sees `<BOXLITE_SECRET:name>`. An HTTPS MITM proxy on the host substitutes the real value, using a per-box CA.
- HTTPS-only. Declaring a secret also opens port 443 to its hosts.
- `docs/reference/configuration.md`

**Persistence**
- Boxes persist across stop/restart with `auto_delete=0`. Default `BoxOptions()` boxes are removed when they stop.
- Clone, export/import, detached boxes.
- Disk snapshots exist. Memory snapshots don't yet.

**Performance (project's own claims)**
- Boot takes 1–2 seconds with a cached image, 5–30 seconds on first pull.
- 512 MiB default memory, about 50–100 MB overhead.

**Coding agents**
- Examples run Claude Code with `CLAUDE_CODE_OAUTH_TOKEN`: `examples/node/claude_in_boxlite.js` and `docs/guides/ai-agent-integration.md`.
- The `box.agents` API in v0.11.0 is **stubbed** (returns `Unsupported`).
- Docker inside a box: the stock kernel lacks bridge/netfilter (https://github.com/boxlite-ai/boxlite/issues/276). Custom kernels are an RC feature.

## Could not verify
- sbx: whether `libsailor` is the default VMM, boot time, the full default allowlist, and GA status.
- BoxLite: independent performance numbers; whether Docker works inside a box in v0.11; the current VMM milestone and default backend; whether `allow_net` matches on DNS, SNI or IP; user complaints beyond the issue search.
