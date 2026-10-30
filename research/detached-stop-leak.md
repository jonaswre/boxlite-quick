# Why does stopping a detached Box from another process leak its shim and VM? (2026-10-30)

Answers [#19](https://github.com/jonaswre/boxlite-quick/issues/19) on the [boxq wayfinder map](https://github.com/jonaswre/boxlite-quick/issues/1). The evidence it starts from is "Found along the way" in the [runtime-lock spike results](https://github.com/jonaswre/boxlite-quick/issues/8#issuecomment-6525877279). This is a throwaway research branch and is never merged.

Code references are to the `boxlite 0.11.0` crate source (`~/.cargo/registry/src/index.crates.io-*/boxlite-0.11.0/`) unless they say otherwise. **[repro]** marks something measured on this host on 2026-10-30 with the [`prototype/runtime-lock-spike`](https://github.com/jonaswre/boxlite-quick/tree/prototype/runtime-lock/prototype/runtime-lock-spike) binary.

## Answer

1. **Cause: an upstream bug that only shows when BoxLite can't put the Box in its own cgroup.** The host doesn't cause it; it only decides whether the bug shows.
   - For a detached Box, BoxLite 0.11.0 has exactly one way to reap the whole process tree: `cgroup.kill` on the Box's cgroup.
   - On this host that cgroup is never created. `mkdir /sys/fs/cgroup/boxlite` fails with EROFS: `t3code.service` has `ProtectControlGroups=yes`, and uid 1500 has no `user@1500.service`.
   - BoxLite logs that failure as a WARN and carries on. On `stop`, the `cgroup.kill` write fails silently and kills nothing.
   - Only the recorded pid, the outer bwrap, is signalled. The inner bwrap (PID-namespace init) and the `boxlite-shim`/libkrun VM survive and are reparented to init.
   - Ruled out:
     - **AppArmor:** it only gates user-namespace creation, which succeeds.
     - **PID-namespace visibility:** the inner tree is fully visible in host `/proc`.
     - **The pidfile:** `shim.pid` correctly records the outer bwrap pid and its start time, as designed.
   - Stopping from "another process" doesn't matter either. The starting process's handler runs the same `stop()` and would leak the same way. In boxq, every stop just happens to come from another process.
2. **Upstream: reported, not fixed, and the issue was closed as not planned.**
   - Issue [boxlite-ai/boxlite#1239](https://github.com/boxlite-ai/boxlite/issues/1239) describes exactly this leak. It was opened 2026-08-14 and closed `NOT_PLANNED` on 2026-10-13 by a maintainer, with no comment.
   - The fix PR, [#1240](https://github.com/boxlite-ai/boxlite/pull/1240), is still **open** with `CHANGES_REQUESTED` (last updated 2026-09-20). It adds a cgroup-independent reap: snapshot the descendants, SIGTERM, wait 4 s, then SIGKILL.
   - [v0.11.0](https://github.com/boxlite-ai/boxlite/releases/tag/v0.11.0) (2026-10-30) is the latest release. `main` is one unrelated commit ahead (`a683b50dc`, infra). No release contains a fix.
3. **Recommendation: a boxq-side reap, kept until upstream ships a fix.**
   - Keep `=0.11.0`; there is no fixed version to bump to.
   - After `LiteBox::stop()`, boxq should kill the Box's surviving tree. Find it by the per-Box shim binary, `<home>/boxes/<id>/bin/boxlite-shim`. Send SIGTERM, wait a bounded time, then SIGKILL.
   - Also sweep before `start`, so a leaked VM can never share a qcow2 with a new one.
   - Don't file a new upstream issue, since #1239 and #1240 already cover it. Optionally, add the data point drafted below to #1240.

## Evidence

### The teardown path in 0.11.0 (code)

- **What gets recorded.** `shim.pid` holds the pid of the **outer** bwrap launcher, plus its `/proc` start time (`src/util/pid_file.rs:18`, a "`<start_time>` (optional)" second line).
  - With the jailer on, the shim runs under `bwrap --unshare-pid --new-session`.
  - So the tree is outer bwrap (host pid ns) → inner bwrap (init of a new pid ns, new session) → `boxlite-shim`, which renames itself `libkrun VM`.
- **No `--die-with-parent` for detached Boxes.** `src/jailer/sandbox/bwrap.rs:75-81` skips it so a `run -d` Box outlives its launcher. That was introduced by [PR #851](https://github.com/boxlite-ai/boxlite/pull/851), merged 2026-06-25.
  - As a result, killing the outer bwrap doesn't take the inner tree with it.
- **`LiteBox::stop`** (`src/litebox/box_impl.rs:684`) runs two steps:
  1. A `Guest.Shutdown` RPC, with a 10 s timeout, which flushes the disk.
  2. `handler.stop()`.
- **`ShimHandler::stop`** (`src/vmm/controller/shim.rs:175-187`) also runs two steps:
  1. `graceful_stop()` sends SIGTERM to `self.pid` only, waits up to 2 s, then SIGKILLs it (`shim.rs:86-160`).
  2. Then `crate::jailer::reap_box(&self.box_id)`.
  - The comment there says the inner pid-ns tree "lives in the box's cgroup, so reap it by id".
- **`RuntimeImpl::remove_box(force)`** (`src/runtime/rt_impl.rs:1006-1030`) goes through `ShimHandler::from_pid(state.pid, ..).stop()`, so it takes the same path.
- **`reap_box` relies only on the cgroup.** It is `cgroup::kill_cgroup(box_id)` (`src/jailer/mod.rs:104-106`).
  - `kill_cgroup` is just `std::fs::write(<cgroup>/cgroup.kill, "1").is_ok()` (`src/jailer/cgroup.rs:166-169`).
  - Its doc says it's "a no-op if the cgroup is gone, already empty, or `cgroup.kill` is unavailable".
  - No other whole-tree mechanism exists.
- **A failed cgroup setup is not an error.**
  - `BwrapSandbox::setup` logs `Cgroup setup failed (continuing without cgroup limits)` at WARN and returns `Ok` (`src/jailer/sandbox/bwrap.rs:47-58`).
  - The `pre_exec` migration into `cgroup.procs` discards its result: `let _ = cgroup::add_self_to_cgroup_raw(..)` (`bwrap.rs:161-168`).
- **Where the cgroup base comes from.**
  - For root, the base is `/sys/fs/cgroup`.
  - Otherwise it is `/sys/fs/cgroup/user.slice/user-<uid>.slice/user@<uid>.service` if that exists, and falls back to `/sys/fs/cgroup` otherwise (`cgroup.rs:63-97`). The comment there says the fallback "will likely fail".

### Reproduction on this host [repro]

- **Host:** Ubuntu 24.04, kernel 6.8.0-147, uid 1500, with the jailer on.
- **Process cgroup:** the process runs in `0::/system.slice/t3code.service`.
  - `systemctl show t3code.service` reports `ProtectControlGroups=yes` and `Delegate=no`.
  - `/sys/fs/cgroup` is mounted `ro`.
  - `loginctl show-user 1500` says "not logged in or lingering", so there is no `user@1500.service`.

`RUST_LOG=boxlite=info runtime-lock-spike up …` logged:

```
WARN boxlite::jailer::sandbox::bwrap: Cgroup setup failed (continuing without cgroup limits) id=5hPLq6rpfCkZ error=cgroup: Failed to create boxlite cgroup at /sys/fs/cgroup/boxlite: Read-only file system (os error 30)
```

Here is the tree before `stop`. All three processes are in `system.slice/t3code.service`, not a Box cgroup. `shim.pid` = 627764.

```
627764 ppid=1      pgid=627764 sid=627764 bwrap       pidns=4026531836 (host)
627765 ppid=627764 pgid=627765 sid=627765 bwrap       pidns=4026532524 (new; init)
627766 ppid=627765 pgid=627765 sid=627765 libkrun VM  pidns=4026532524
```

`runtime-lock-spike stop …` ran from a second process:

```
INFO Recovered running box from PID file box_id=5hPLq6rpfCkZ pid=627764
INFO Attached to existing VM process box_id=5hPLq6rpfCkZ pid=627764
INFO Box stopped box_id=5hPLq6rpfCkZ elapsed_ms=2129
```

`elapsed_ms=2129` is the 2 s `graceful_stop` timeout followed by a SIGKILL of the outer bwrap. After the stop:

```
627765 ppid=1      bwrap       rss 1.5 MB
627766 ppid=627765 libkrun VM  rss 107 MB
```

The workaround mechanism works. `kill -TERM 627766` targets the shim, whose `/proc/<pid>/exe` is `…/boxes/5hPLq6rpfCkZ/bin/boxlite-shim`, and the whole tree was gone within about 100 ms. The shim installs a SIGTERM handler that runs `Guest.Shutdown` (flush) and then re-raises SIGTERM ([`src/shim/src/main.rs` at v0.11.0, lines 218-318](https://github.com/boxlite-ai/boxlite/blob/v0.11.0/src/shim/src/main.rs#L218-L318)). Once the shim exits, the inner bwrap (pid-ns init) exits and the namespace is torn down. The test Box was then removed, and none of its processes remain.

### The leak is worse than a memory cost [repro, read-only]

These are leftovers from earlier spike runs, which I didn't create and didn't touch. They are still running and are listed for the parent session to clean up.

| shim pid | Box | Box dir | RSS |
|---|---|---|---|
| 430580 | `aD6AFykim1Fa` | deleted | 894 MB |
| 434713 | `aD6AFykim1Fa` | deleted | – |
| 580479 | `aD6AFykim1Fa` | deleted | 373 MB |
| 583350 | `MLs3S7qMD4z1` | exists | 927 MB |
| 609209 | `KTwbeIdZ1Wrx` | deleted | 107 MB |

Each one has a parent bwrap reparented to pid 1. **Three live VMs of the same Box (`aD6AFykim1Fa`) each hold an open fd on a `disk.qcow2`.** They come from repeated `up`/`stop` cycles in the earlier spikes: each restart booted a new VM while the leaked one still ran. #1239 calls this out as a qcow2 corruption risk. Two of them are in `system.slice/qemu-guest-agent.service` rather than `t3code.service`, which is a different launcher but has the same lack of a delegated cgroup.

### Which hosts are affected (upstream #1239, measured there)

[#1239](https://github.com/boxlite-ai/boxlite/issues/1239) shows the trigger is the cgroup of the process that starts the Box. Distro, kernel and AppArmor don't matter.

- **Affected:**
  - SSH or tty logins (`session-N.scope`);
  - WSL (`/init.scope`);
  - cron or system units (`system.slice/…`, like this host);
  - containers or CI without systemd.
- **Not affected:**
  - a desktop terminal or any other `user@<uid>.service` unit;
  - root.

The two affected modes differ:

- **No delegated base** (this host): the `mkdir` itself fails.
- **A caller outside `user@<uid>.service`:** the `mkdir` succeeds, but migration into `cgroup.procs` fails with EACCES. cgroup-v2 delegation needs write access on the common ancestor, which root owns. The cgroup then exists but is empty, `cgroup.kill` "succeeds", and nothing dies.

So boxq used over SSH or from an agent harness leaks, while the same boxq run from a GNOME terminal does not.

### Upstream status (read via `gh`, 2026-10-30)

- **[#851](https://github.com/boxlite-ai/boxlite/pull/851)** "fix(jailer): skip --die-with-parent for detached boxes", merged 2026-06-25. This is where the leak comes from.
- **[#1239](https://github.com/boxlite-ai/boxlite/issues/1239)** "`stop` leaks a detached box's VM wherever the box cgroup cannot be populated". Opened 2026-08-14 by lilongen.
  - Labelled `Runtime & SDK` and `Networking & Security`.
  - Closed `NOT_PLANNED` on 2026-10-13 by DorianZheng with no comment.
  - Its "Suggested fix" is the runc/crun approach: snapshot the launcher's descendants and signal them directly, with `cgroup.kill` as hardening.
- **[#1240](https://github.com/boxlite-ai/boxlite/pull/1240)** "fix(vmm): reap the box process tree when cgroup kill is unavailable", **open**. "Fixes #1239".
  - What it does:
    - snapshots the descendants as (pid, start-time);
    - `terminate_and_wait`, SIGTERM with a 4 s grace;
    - `reap_box`;
    - `reap_pids`, a SIGKILL of survivors;
    - teardown identity anchored on `shim.pid`.
  - ltstriker requested changes on 2026-08-26, over a PID-reuse hazard with stale DB pids. The author pushed a fix (`5a279d64`) on 2026-09-15. There has been no re-review since; it was last updated 2026-09-20.
- **Related open PRs, neither a fix for this:**
  - [#1462](https://github.com/boxlite-ai/boxlite/pull/1462) moves teardown off Tokio workers.
  - [#1463](https://github.com/boxlite-ai/boxlite/pull/1463) closes a PID-reuse TOCTOU via pidfd.
- **[v0.11.0](https://github.com/boxlite-ai/boxlite/releases/tag/v0.11.0)**, released 2026-10-30, is the latest release.
  - `git compare v0.11.0...main` shows one commit (`a683b50dc fix(infra): preserve runner sizing and configure data disks (#2236)`).
  - No commits since 2026-10-01 touch `vmm/controller/shim.rs`, `jailer/cgroup.rs` or `jailer/sandbox/bwrap.rs`. One unrelated commit (`1ed923bf8`, bind-mount grants) touches `jailer/mod.rs`.

## Recommendation for boxq

**1. Keep the pin.** No version fixes this. Watch #1240. When a release contains it (or an equivalent cgroup-independent reap), bump and delete the boxq workaround behind the real-Box CI test.

**2. Workaround: boxq reaps the Box's tree itself.** It's small and safe, because the shim binary is copied per Box (`<home>/boxes/<id>/bin/boxlite-shim`, the "Firecracker pattern" `jailer` log line). Matching `/proc/<pid>/exe` against that path therefore identifies exactly one Box's VM.

- **`stop`:**
  1. Call `LiteBox::stop()` as now. It flushes through `Guest.Shutdown`, kills the outer bwrap and marks the Box Stopped.
  2. Then scan `/proc` for processes owned by the same uid whose `exe` is `<home>/boxes/<id>/bin/boxlite-shim`. Strip a trailing ` (deleted)` before comparing.
  3. Open a pidfd for each, send SIGTERM, and wait up to about 4 s; that's #1240's measured flush budget.
  4. Then SIGKILL survivors and their parent bwrap (the pid-ns init) through the pidfd.
  5. Using a pidfd, or a start-time recheck, avoids signalling a recycled pid.
- **`rm`:** run the `stop` path, reap included, before `remove`. `remove` deletes `boxes/<id>/`, but the `exe` link still matches with the ` (deleted)` suffix.
- **`up` / start:** run the same reap for the Box's id before starting it. A leaked VM must never share `disk.qcow2` with a new one, which is the corruption case seen above.
- **Test:** the real-Box CI test should assert that no process with that `exe` survives `boxq stop` and `boxq rm`. GitHub-hosted runners run steps outside `user@.service`, so CI is an affected host and the test exercises the workaround.
- **Out of scope for now:** a global orphan sweep, meaning shims under `<home>/boxes/*/` whose Box is not Running. It's cheap to add later as a `boxq doctor`/`gc`. It would also clean up today's leftovers, but the per-Box reap covers every leak boxq itself can cause.

**3. Upstream.** Don't file a new issue: #1239 already describes the bug and #1240 is the fix, so a new issue would be a duplicate of one a maintainer closed. If jonaswre wants to push it along, this is a draft comment for **#1240** (not posted):

> Another affected configuration, on 0.11.0 with the jailer on: the caller runs in a systemd *system* service with `ProtectControlGroups=yes`, uid with no `user@<uid>.service`. Here the `mkdir` itself fails (`Failed to create boxlite cgroup at /sys/fs/cgroup/boxlite: Read-only file system (os error 30)`, logged only at WARN by `BwrapSandbox::setup`), so `reap_box` writes to a nonexistent `cgroup.kill` and `stop` reaps only the outer bwrap. Observed consequence: repeated stop/start cycles of one box left **three** live `boxlite-shim` processes, each with an open fd on that box's `disk.qcow2`. SIGTERM to the leaked shim tears the tree down cleanly (~100 ms; the shim's SIGTERM handler flushes via `Guest.Shutdown`), which matches the approach in this PR. Is there anything blocking a re-review after 5a279d64? Separately: would you accept failing (or at least surfacing) a start whose box cgroup could not be populated, rather than WARN-and-continue?

## Implied build ticket

**boxq reaps a Box's leaked VM tree on stop/rm/start.** On Linux, after `LiteBox::stop()` and before any start or remove, boxq finds processes whose `/proc/<pid>/exe` is `<home>/boxes/<id>/bin/boxlite-shim`. It sends SIGTERM, waits up to about 4 s and then SIGKILLs them via pidfd, and the real-Box CI test asserts that none survive. Remove the workaround once a BoxLite release reaps the tree itself (watch boxlite-ai/boxlite#1240).
