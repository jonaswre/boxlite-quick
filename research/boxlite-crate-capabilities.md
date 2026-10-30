# What the `boxlite` crate (v0.11.0) supports for boxq's Sandbox needs (2026-10-30)

Answers [#2](https://github.com/jonaswre/boxlite/issues/2) for the [boxq map](https://github.com/jonaswre/boxlite/issues/1). It builds on the [landscape briefing](https://github.com/jonaswre/boxlite/blob/research/sbx-boxlite-landscape/research/sbx-boxlite-landscape.md) and doesn't repeat it.

**Source.** Everything below comes from reading the source of [boxlite-ai/boxlite](https://github.com/boxlite-ai/boxlite) at commit `a683b50` (2026-10-30, workspace version `0.11.0`, `Cargo.toml:40`). Nothing was run. A citation `path:line` means `https://github.com/boxlite-ai/boxlite/blob/a683b50dcb22dffb4daa24b9451c4b48be056abb/<path>#L<line>`. Files under `gvproxy-bridge/` are in `src/deps/libgvproxy-sys/gvproxy-bridge/`. "libkrun" means BoxLite's fork pinned at `boxlite-ai/libkrun@e12b9b3` (the submodule in `.gitmodules`).

## One-liner

The crate covers almost everything boxq needs, with one blocker. **Only one `BoxliteRuntime` per `BOXLITE_HOME` can be open at a time, enforced by an exclusive `flock`.** A long `boxq run` that holds its runtime open therefore locks out every other `boxq` command on the same home. Everything else works, or works with a configuration workaround. The remaining gaps that need upstream patches are about granularity (no port-scoped egress rules, no secret updates on an existing Box), not missing features.

## Answers per bullet

### 1. Same-path mounts (Workspace, plus the worktree's common `.git`)

**Yes.** Use `BoxOptions.volumes: Vec<VolumeSpec>` with `VolumeSpec::bind_mount(host, guest)` (`src/boxlite/src/runtime/options.rs:836-866`).
- `guest_path` is any absolute path you choose, so it can equal the host path. Several entries are allowed, so the Workspace and the common `.git` directory can both be mounted read-write at their own paths.
- The host path is canonicalized, so symlinks are resolved. Each volume gets a virtiofs tag `uservol{i}` (`src/boxlite/src/litebox/init/types.rs:50-129`).
- Mounts go into the OCI spec in vector order, after the standard mounts (`/proc`, `/dev`, `/sys`, a tmpfs `/tmp`) (`src/guest/src/container/spec.rs:188-219`, `:686-698`). If one mount is nested inside another, list the parent first.
- **Don't use `BoxOptions.mounts` (`MountSpec`).** The local runtime rejects any typed mount with `Unsupported` (`src/boxlite/src/runtime/rt_impl.rs:1785-1791`).
- Mounts are fixed when the Box is created. Live mount hot-plug is a later VMM milestone (M6, `docs/contributing/architecture/vmm/README.md`). The only setting you can change on an existing Box is outbound network (see §5). Reusing a Box with `get_or_create` does not compare mounts (`rt_impl.rs:543-590`), so changing a Box's mounts means recreating it.

### 2. File ownership over virtiofs

**Files the guest writes land on the host owned by the user running boxq.** The mechanism:
- The shim runs as the calling user, inside `bwrap --unshare-user` with no capabilities (`src/boxlite/src/jailer/bwrap.rs:326`).
- So libkrun's virtiofs server has no `CAP_SETUID`. Its `set_creds` accepts a request as guest uid 0 or as the shim's own uid, and creates the file as the shim user. **It rejects every other uid/gid with `EPERM`** (libkrun `src/devices/src/virtio/fs/linux/passthrough.rs:833-862`, `:417`).
- To bridge that, the guest applies an automatic **idmapped mount** to each writable volume whose host owner differs from the container user. It is a full-range swap of host owner uid/gid ↔ container uid/gid (`src/guest/src/container/lifecycle.rs:150-180`). The owner is read from `stat` of the mount root (`types.rs:99-110`).
- If idmapping fails (`ENOSYS`/`EPERM`), it is skipped with only a warning (`src/guest/src/storage/idmap.rs:117-130`). After that, writes from a uid that is neither 0 nor the host uid fail.

**How boxq makes ownership match.** Run the Agent either as root, or as a guest user whose uid/gid equal the host user's. Set this with `BoxOptions.user = "UID:GID"` or `BoxCommand::user`. Then no idmap is needed and writes come straight through as the host user. Two side effects:
- `chown` to any other uid inside the guest fails with `EPERM`.
- The idmap is computed for the Box's **main** user only (`lifecycle.rs:147-150`), so an `exec` as a different uid doesn't get one.

### 3. Persistence and reattach

| Need | How | Source |
| --- | --- | --- |
| Persistent Box | `auto_delete: Some(0)`. **The default removes the Box on stop**: `auto_delete: None` falls back to the deprecated `auto_remove`, which defaults to `true`. | `options.rs:414-437`, `:586-588`, `:627-637` |
| Stable key | Pass a name to `runtime.create(opts, Some(name))` or `get_or_create(opts, Some(name))`. `get(id_or_name)` looks up the exact ID, then the exact name, then an ID prefix. IDs are 12-character base62. No name validation was found, but a hashed name such as `boxq-<hash(path)>` is safer than the raw path. | `src/boxlite/src/runtime/core.rs:309-367`, `src/boxlite/src/litebox/manager.rs:102-125`, `runtime/id.rs` |
| Metadata | Can't be stored. `BoxInfo.labels` exists but is always empty, and `BoxOptions` has no labels field. boxq must encode the Workspace in the name, or keep its own index. | `src/boxlite/src/runtime/types.rs:489-490`, `:581` |
| Detached lifetime, no daemon | `detach: true` requires `auto_delete = 0`. The Box outlives the creating process (clean exit, panic or `SIGKILL`); any process can reattach with `runtime.get(...)`. With `detach: false` (the default), the Box stops when its runtime is dropped, but its disk persists and it restarts with `start()`. | `options.rs:444-455`, `:659-665`; `jailer/sandbox/bwrap.rs:76`, `:228-262` (`--die-with-parent` only for foreground Boxes) |
| stop / start / remove | `LiteBox::stop()`; `start()` (idempotent, and it restarts a Stopped Box; `exec()` also starts one implicitly); `runtime.remove(name, force)`; `list_info()` for `boxq ls`. | `src/boxlite/src/litebox/mod.rs:100-145`, `core.rs:350-367` |
| Keeping the Box alive | **The VM powers off when the main command exits.** boxq must set `cmd` to a long-lived idle process (for example `sleep infinity`) and run Claude through `exec`. | `src/guest/src/service/container.rs:413-480` |

**Blocker: one runtime per home.** `BoxliteRuntime::new` takes an exclusive non-blocking `flock` on `<home>/.lock` for as long as the runtime lives. A second process gets the error "Another BoxliteRuntime is already using directory" (`src/boxlite/src/runtime/lock.rs:1-80`, `rt_impl.rs:277-283`). The image store and base disks are single-process by design and rely on this lock (`src/boxlite/src/images/image_disk.rs:30`). An `exec` needs a live runtime. So while one `boxq run` is attached to Claude:
- `boxq ls`, `stop`, `rm` and `shell` fail;
- a second `boxq run` in another Workspace fails.

Upstream knows about this: boxlite-ai/boxlite#258, "runtime ReadOnly mode", is open with no implementation. Options for boxq:
- (a) **One `BOXLITE_HOME` per Sandbox.** Each Sandbox gets its own image cache, so pulls and base disks are duplicated. `boxq ls` has to scan the homes, and still can't open a home that is in use.
- (b) **SSH instead of `exec`.** Create the Box detached, enable guest SSH (`LiteBox::ssh().configure`, `docs/guides/ssh.md`), publish a `127.0.0.1` port, close the runtime, then `ssh` in. Published-port forwards live in gvproxy inside the shim (`src/boxlite/src/litebox/init/tasks/port_publish.rs:1-12`), so they should survive the CLI exiting. **This is unverified; prototype it.**
- (c) **Upstream patch:** a shared or read-only runtime mode.
- `boxlite serve` (the REST server) would also solve it, but it is a daemon, which the map's settled decisions exclude.

### 4. Interactive exec with a TTY

**Yes.**
- Build the command with `BoxCommand::new(cmd).args(..).env(..).working_dir(..).user(..).tty(true)` (`src/boxlite/src/litebox/exec.rs:31-117`).
- `LiteBox::exec` returns an `Execution`. It offers `stdin()`, `stdout()` and `stderr()` streams, `wait()`, `signal()`, `kill()` and `resize_tty(rows, cols)` (`exec.rs:211-290`).
- The crate does **not** manage the host terminal. boxq has to replicate the raw-mode guard, the stdin pump, the `SIGWINCH` → `resize_tty` relay and signal forwarding from the boxlite CLI. That code is about 250 lines in `src/cli/src/terminal/mod.rs:22-250`, in a binary crate, so it can't be reused as a library.
- `TERM` isn't set for `exec`; only the SSH service sets one. Pass `TERM` explicitly.
- `attach(AttachOptions::main())` attaches to the main command. Reattaching to an `exec` session by ID works on REST only (`litebox/mod.rs:115-137`).

### 5. Egress

**Semantics:**

| Setting | Effect |
| --- | --- |
| `network: NetworkSpec::Enabled { allow_net }` | The outbound policy. |
| Empty `allow_net` | **Full access** (`options.rs:1288-1307`). |
| Non-empty `allow_net` | Only the listed destinations are allowed; everything else is refused at connect time with a TCP RST. |
| `Disabled` | No NIC at all. |

- There is no "enabled but allow nothing" setting. Because boxq always has a built-in allowlist, it just must **never** pass an empty list. That includes a list emptied by user config, and the "secrets only" case: secrets with an empty `allow_net` still means full egress.
- The allowlist can be replaced on a running Box with `box.network().update_outbound(OutboundNetworkConfig{..})`. It applies to new flows and is persisted across stop/start (`docs/guides/networking.md`, "Updating a running box").

**Matching:**
- Rules are exact hostname, `*.suffix` wildcard, IPv4 or IPv4 CIDR. `host:port` and IPv6 are rejected (`options.rs:1117-1156`).
- IP and CIDR rules match the destination IP, for both TCP and UDP (`gvproxy-bridge/allow_net_filter.go`).
- **Hostname rules match the TLS SNI (port 443) or HTTP `Host` header (port 80) only.** A hostname rule never authorizes any other port. With a hostname-only allowlist, all UDP is denied (`gvproxy-bridge/forked_tcp.go:64-87`, `options.rs:1264-1281`).
- After a hostname match, the gateway **resolves the name itself with the host resolver (`getaddrinfo`) and dials it**. It ignores the guest's destination IP, so editing `/etc/hosts` in the guest doesn't help (`gvproxy-bridge/egress_dialer.go:1-17`, `:59-82`).
- DNS is not filtered. Guest queries are forwarded to the host's resolver (`gvproxy-bridge/main.go:220-244`). DNS exfiltration therefore stays possible.
- Wildcard `*.example.com` in `allow_net` matches subdomains at any depth but not the apex (`allow_net_filter.go:140-156`).

**Host loopback:**
- The guest reaches the host's `127.0.0.1` only through `host.boxlite.internal` = `192.168.127.254`, which is NATed to the host's 127.0.0.1 (`src/boxlite/src/net/constants.rs`, `gvproxy-bridge/main.go:262-268`).
- Under a non-empty `allow_net` that address is **blocked unless listed** (`main.go:245-260`).
- A hostname that resolves to loopback, a private range, CGNAT, link-local or multicast is refused unless an IP/CIDR rule covers it. Link-local, including the cloud metadata address, is always refused (`egress_dialer.go:218-258`).
- So the map's rule "host loopback blocked unless explicitly opened" holds by default. Opening it means listing `192.168.127.254`, which opens **every** loopback port.

**Tailscale:**
- The gateway dials from ordinary host sockets, so a 100.x address or MagicDNS name is reached through the host's routing table and resolver.
- But because 100.64.0.0/10 is CGNAT, **a hostname rule alone is refused**. boxq must also add an IP or CIDR rule: preferably the exact tailnet IP, resolved on the host when the Sandbox is created.
- A tailnet HTTP endpoint on a port other than 80 can only be allowed through an IP rule, and that rule opens every port and UDP on that IP.

### 6. Secrets

**API:** `BoxOptions.secrets: Vec<Secret { name, hosts, placeholder, value }>` (`options.rs:502-563`). `hosts` takes exact names or `*.x` (wildcards match one level only, `gvproxy-bridge/mitm.go:229-256`). The guest also gets `BOXLITE_SECRET_<NAME>=<placeholder>` in its environment (`Secret::env_pair`). For a tailnet `ANTHROPIC_BASE_URL`:
- **The URL must be HTTPS on port 443.** Interception only happens for `destPort == 443` with a matching SNI (`gvproxy-bridge/forked_tcp.go:64-71`, `:129-134`). `https://host:8443` would not be intercepted.
- The secret host is dialed **by name**. The upstream certificate is checked against the **host's** system roots (`mitm.go:203-218`); a Tailscale `*.ts.net` certificate passes.
- A secret host is reachable on 443 even without its own `allow_net` rule. Under a non-empty allowlist, though, a 100.x answer is still refused unless an IP or CIDR rule covers it (`docs/reference/configuration.md:120-145`).
- Placeholders are replaced in headers, URL query and a streamed body. Responses are flushed immediately (`FlushInterval: -1`), so SSE works (`mitm_proxy.go:24-70`, `mitm.go:179-198`). Claude Code's `x-api-key` header is covered.
- **Secrets can't be changed on an existing Box.** No update API exists; only outbound network is mutable (`litebox/network.rs:401-420`). Rotating the key or changing the base-URL host means recreating the Box.
- The real secret value is serialized into BoxLite's SQLite database under `BOXLITE_HOME` (`options.rs:528-534`).

**CA trust:**
- Each Box gets its own ECDSA P-256 CA, persisted in the Box's directory and valid for 10 years (`src/boxlite/src/net/ca.rs`).
- On every container init it is **appended to `/etc/ssl/certs/ca-certificates.crt` in the container rootfs** (`src/guest/src/service/container.rs:262-280`, `src/guest/src/ca_trust.rs`). That file must already exist, so the base image needs Debian's `ca-certificates`.
- If a setup hook runs `update-ca-certificates`, the bundle is regenerated without the CA until the next start.
- Claude Code's native build trusts `bundled,system` by default and reads the OS store; npm installs need Node ≥ 22.15 (https://code.claude.com/docs/en/network-config). Other Node tools only see the CA through `NODE_EXTRA_CA_CERTS` or `NODE_USE_SYSTEM_CA`.
- boxq should set `NODE_EXTRA_CA_CERTS=/etc/ssl/certs/ca-certificates.crt` as a safeguard. Only traffic to secret hosts is intercepted, so other TLS is unaffected.

### 7. Images from GHCR

**Yes.** `RootfsSpec::Image("ghcr.io/owner/img:tag")` (`options.rs:812-823`):
- A fully qualified reference is pulled directly. Unqualified names fall back to `docker.io` (`docs/guides/image-registry-configuration.md:7-13`).
- Private images take `BoxliteOptions.image_registries` with `ImageRegistry::https("ghcr.io").with_basic_auth(..)` or `.with_bearer_auth(..)` (`options.rs:98-160`). Pulls use `oci-client` 0.15.
- An existing Box keeps its copy-on-write disk built from the image it was created with. Pulling a new tag affects new Boxes only.

### 8. Backend and API stability

- **libkrun is the only working backend.** `VmmKind` defaults to `Libkrun` (`src/boxlite/src/vmm/mod.rs:73-77`). `Firecracker` is an enum value with no registered engine (`vmm/registry.rs:96-98`).
- The native VMM crates (`src/vmm`, `src/hypervisor`) exist but aren't linked into the shim or the `boxlite` crate. libkrun stays the default until milestone M5 (`docs/contributing/architecture/vmm/README.md`).
- Crate features default to `embedded-runtime, krunfw, e2fsprogs, bubblewrap`. Building from crates.io downloads prebuilt runtime binaries (`src/boxlite/Cargo.toml:22-31`, `build.rs:585-600`).
- **Stability is low.** There are 11 releases in the last 2 months (0.10.0 on 2026-08-28 through 0.11.0 on 2026-10-30). Breaking changes (`!`) ship in **patch** releases, for example:
  - 0.10.6 removed `CopyOptions.include_parent`;
  - 0.10.7 changed tunnel `connect()` semantics;
  - 0.10.8 changed the REST API.
  
  Option fields are being renamed in place (`auto_remove` → `auto_delete`, `auto_pause` → `auto_stop`). Sources: `gh release view` for each tag, and crates.io.
- Pin boxlite to an exact version (`=0.11.0`) and upgrade deliberately.
- `BoxOptions` is a plain struct with public fields and `#[serde(default)]`, so new fields break struct literals unless you use `..Default::default()`.

## Gaps that would need an upstream patch

1. **One runtime per `BOXLITE_HOME`** (exclusive `flock`). Concurrent `boxq` processes on one home are impossible. The workarounds are per-Sandbox homes or SSH into a detached Box. A real fix needs upstream (#258).
2. **No port-scoped egress rules.** `host:port` is rejected, an IP rule opens all ports and UDP, and hostname rules only work on 80/443. The map's plain-HTTP rule ("only that host:port is opened") **cannot be expressed exactly**; the closest is the exact IP, all ports. Opening host loopback likewise opens all of 127.0.0.1.
3. **Secret injection only on port 443.** A base URL on another HTTPS port can't use placeholders.
4. **No way to update secrets, env, mounts or resources on an existing Box.** Only `update_outbound` exists, so key rotation or a base-URL change means recreating the Box.
5. **No labels/metadata on Boxes** (`BoxInfo.labels` is always empty). This is minor: encode the key in the name.
6. **No hostname → private/CGNAT exception by name.** A tailnet host needs an IP/CIDR rule that boxq resolves itself. This is minor and can be worked around.
7. **DNS isn't filtered** under `allow_net`. Leaving it as a known limitation is fine unless the Egress Policy requires blocking DNS exfiltration.

## Could not verify (prototype these)

- Whether guest idmapped mounts on virtiofs actually work with the bundled libkrunfw kernel. The fallback is to run as root or as the host uid.
- Whether published ports and SSH on a detached Box keep working after the creating runtime exits (workaround (b) in §3).
- That the `<BOXLITE_SECRET:…>` placeholder is accepted by Claude Code as an `ANTHROPIC_API_KEY` value. A custom `placeholder` such as `sk-boxq-placeholder` sidesteps any format check.
- Whether a Box name containing `/` causes trouble anywhere: CLI, on-disk paths, or logs.
