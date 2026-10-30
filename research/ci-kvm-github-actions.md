# Can boxq's integration tests boot BoxLite micro-VMs in GitHub Actions? (2026-10-30)

Answers [#4](https://github.com/jonaswre/boxlite/issues/4) on the [boxq wayfinder map](https://github.com/jonaswre/boxlite/issues/1). This is a throwaway research branch and is never merged. Claims marked **[CI log]** come from reading job logs of public GitHub Actions runs, not docs.

## Answer

**Yes, on standard x86_64 hosted runners (`ubuntu-24.04` / `ubuntu-latest`).** They expose `/dev/kvm`. Two setup steps are needed:

1. Make `/dev/kvm` accessible to the `runner` user, using a udev rule or `chmod`.
2. On Ubuntu 24.04, lift AppArmor's unprivileged-userns restriction so BoxLite's bwrap jailer can start.

BoxLite's own CI boots a real Box this way on a free 4-vCPU `ubuntu-24.04` runner: `boxlite run --rm alpine:latest echo ok` prints `ok` about 4.6 s after the step starts.

**No on `ubuntu-24.04-arm`.** The arm64 hosted fleet has no `/dev/kvm`. This doesn't affect boxq, which is x86_64-only.

You don't need a larger runner. jonaswre/boxlite is public, so standard runners are free and unlimited, with 4 vCPU and 16 GB RAM.

## Evidence

### GitHub's position (primary)

- **2-vCPU Linux runners support KVM hardware acceleration as of 2024-04-02.** Before that it needed 4+ vCPU runners, and larger runners got it on 2023-02-23. GitHub's documented setup is a udev rule: `KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"` in `/etc/udev/rules.d/99-kvm4all.rules`, then `udevadm control --reload-rules` and `udevadm trigger --name-match=kvm`.
  - https://github.blog/changelog/2024-04-02-github-actions-hardware-accelerated-android-virtualization-now-available/
  - https://github.blog/changelog/2023-02-22-hardware-accelerated-android-virtualization-on-actions-windows-and-linux-larger-hosted-runners/
- The hosted-runner reference says "GitHub-hosted Linux runners support hardware acceleration for Android SDK tools". It doesn't mention KVM by name. https://docs.github.com/en/actions/reference/runners/github-hosted-runners
- **Standard runner specs** (same page):
  - Public repos: x64 and arm64 Linux both get 4 CPU, 16 GB RAM, 14 GB SSD.
  - Private repos: 2 CPU, 8 GB RAM.
  - "Use of the standard GitHub-hosted runners is free and unlimited on public repositories."
- **Caveat: KVM is framed around Android emulators.** The hosted runners are Azure VMs, so `/dev/kvm` there is nested virtualization. A runner-images maintainer states that "nested virtualization isn't officially supported on GitHub-hosted runners in general". In practice it works on x64, but it carries no SLA. https://github.com/actions/runner-images/issues/14062
- **arm64 has no KVM:** "the ARM64 hosted fleet doesn't surface a nested KVM device … a host/hypervisor limitation, not something the runner image can add". The issue was closed 2025-12-03 without a fix. https://github.com/actions/runner-images/issues/14062

### BoxLite's own CI (boxlite-ai/boxlite, read via `gh api`)

- **Hosted runner, real Box boot.** The `Nix flake build` job in [`.github/workflows/test.yml`](https://github.com/boxlite-ai/boxlite/blob/main/.github/workflows/test.yml) runs on `ubuntu-latest`. It has two steps:
  - `Enable KVM and unprivileged user namespaces`: `sudo chmod a+rw /dev/kvm` and `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`.
  - `Boot a box`: `./target/debug/boxlite run --rm alpine:latest echo ok`, with a 10-minute timeout.
  - It was added in [PR #2094](https://github.com/boxlite-ai/boxlite/pull/2094) (merged 2026-10-23). The PR says the job "passed in 9 min (8.5 min compiling); the box printed `ok`". It is non-blocking (not in `Test (conclusion)`) and runs only when CLI inputs change.
- **[CI log]** [Run 39938215272, job 120362963749](https://github.com/boxlite-ai/boxlite/actions/runs/39938215272/job/120362963749), the merge commit of #2094:
  - Runner: Azure, centralus, image `ubuntu-24.04` version 20261012.334.1, Ubuntu 24.04.5.
  - `Boot a box` started at 10:28:07.78 and printed `ok` at 10:28:12.37, then `[boxlite] Auto-stopping non-detached box`.
  - The job's Nix installer action also wrote the same `99-kvm4all.rules` udev rule ("Enabled KVM").
- **Hosted KVM tests on every Rust change.** The `Rust Tests (linux-x64-gnu)` job runs `sudo chmod a+rw /dev/kvm` and then `make coverage:vmm:kvm` and `make coverage:vmm:boot` on `ubuntu-latest`. These exercise BoxLite's new native VMM against real KVM, not the libkrun runtime.
  - The workflows README says: "Missing `/dev/kvm` access fails this qualification." https://github.com/boxlite-ai/boxlite/blob/main/.github/workflows/README.md
  - **[CI log]** The scheduled run of 2026-10-26 (job 121477062692) passed both KVM steps.
  - The KVM steps are gated to `linux-x64-gnu`. The `linux-arm64-gnu` job on `ubuntu-24.04-arm` skips them, which is consistent with arm64 having no KVM.
- **Full integration suite on a self-hosted runner.** [`e2e-local.yml`](https://github.com/boxlite-ai/boxlite/blob/main/.github/workflows/e2e-local.yml) runs `make test:integration` on a self-hosted runner:
  - The runner is a persistent GCP VM, `c3-highcpu-4` with `--enable-nested-virtualization`. A hosted job starts it via OIDC, and it is stopped after each run.
  - Its [runbook](https://github.com/boxlite-ai/boxlite/blob/main/docs/contributing/development/e2e-local.md) says "GitHub-hosted runners cannot expose `/dev/kvm`". **That statement is stale.** BoxLite's own `test.yml` contradicts it.
  - The real reasons the setup is still worth it:
    - Persistent build and image caches on disk. The runbook says "a cold build is much slower" and "a first run can come close to the 50-minute timeout".
    - A test that needs nested KVM *inside* the Box, probed via `/sys/module/kvm_*/parameters/nested`.
    - Gating forks on a maintainer-applied label.
- **Ubuntu 24.04 AppArmor gotcha.** The symptom is "Timeout waiting for guest ready (30s)", or a Box that only starts with `sudo`. The cause is Ubuntu 24.04's `kernel.apparmor_restrict_unprivileged_userns=1` blocking the bundled bwrap. The fixes are:
  - Install a `bwrap-userns-restrict` AppArmor profile (recommended).
  - Set the sysctl to 0 (quick).
  - Disable the jailer.
  - https://github.com/boxlite-ai/boxlite/blob/main/docs/guides/troubleshooting.md
- The `boxlite` crate's default features are `embedded-runtime`, `krunfw`, `e2fsprogs`, and `bubblewrap`. So a boxq build embeds the runtime and bundles bwrap. Expect a long first compile and cache `target/`. https://github.com/boxlite-ai/boxlite/blob/main/src/boxlite/Cargo.toml

### Other VMM projects

- **libkrun** ([`containers/libkrun` `integration_tests.yml`](https://github.com/containers/libkrun/blob/main/.github/workflows/integration_tests.yml)) boots real libkrun VMs on hosted `ubuntu-26.04`.
  - It uses the same udev rule plus `sudo usermod -a -G kvm $USER`.
  - **[CI log]** Recent runs mostly succeed (5 of 8 on 2026-10-29/30). The failures are on one feature branch.
- **systemd/mkosi** ships a reusable action ([`action.yaml`](https://github.com/systemd/mkosi/blob/main/action.yaml)). It sets the `kvm`, `vhost-vsock`, and `vhost-net` nodes to 0666 via udev and tmpfiles, `modprobe`s them, and `chmod 666`s them. It's a good reference if vsock or vhost devices ever matter.
- **Firecracker** runs its CI on Buildkite (`.buildkite/` in https://github.com/firecracker-microvm/firecracker), not Actions. It isn't evidence either way for hosted runners.

## Minimal workflow for boxq

Not yet run against boxq, which doesn't exist yet. It combines the two setups proven above.

```yaml
name: integration
on: [push, pull_request]
jobs:
  boot-a-box:
    runs-on: ubuntu-24.04          # x86_64. NOT ubuntu-24.04-arm (no /dev/kvm)
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2   # boxlite embeds its runtime; first build is slow
      - name: Give the runner user /dev/kvm
        run: |
          echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
            | sudo tee /etc/udev/rules.d/99-kvm4all.rules
          sudo udevadm control --reload-rules
          sudo udevadm trigger --name-match=kvm
          test -r /dev/kvm && test -w /dev/kvm
      - name: Let BoxLite's bwrap jailer use user namespaces (Ubuntu 24.04)
        run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
      - run: cargo test --locked -- --include-ignored
```

Notes:

- **Build packages.** BoxLite's own hosted Rust job installs `jq libseccomp-dev protobuf-compiler util-linux`. It's unverified whether a crates.io consumer of `boxlite` needs any of them. Add them if the build asks.
- **Image registry.** Pull the boxq base image from GHCR rather than Docker Hub, to avoid anonymous pull rate limits. Upstream also replays images from a local registry for this reason.
- **Disk.** The runner's disk is 14 GB. Watch `target/` plus image caches.
- **Tests that can't run in CI.** Nested KVM inside a Box won't work on hosted runners. boxq's MVP doesn't need it, since Docker-in-Sandbox is out of scope.

## If hosted runners stop working: cheapest fallbacks, in order

1. **Your own Linux/KVM workstation as a self-hosted runner.** boxq's target host already is one. It's free, gives persistent caches, and needs no nested virt. The risk: never let fork PRs run on it. Use `pull_request_target` with a label gate like upstream does, or skip PRs from forks.
2. **A stopped-between-runs cloud VM with nested virtualization.** This is upstream's pattern, a GCP `c3-highcpu-4` with `--enable-nested-virtualization` started and stopped via OIDC. You pay only for the minutes it runs plus disk, at the cost of more moving parts (GitHub App token, runner re-registration).
3. **Larger GitHub runners buy nothing here.** KVM is already on standard runners, and larger runners are paid. They only help with build time.
4. **Local-only integration tests** (`cargo test -- --ignored` on the dev box). This is the fallback if all CI options are rejected.
