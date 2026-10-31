# Testing

`cargo test --locked` runs the tests that need no KVM. The real-Box tests in `tests/real_box/` boot real Sandboxes, so they're `#[ignore]`d. Run them with `scripts/real-box`, in the foreground: it ends within 10 minutes, so you get the result in the same command. Don't push a run into the background, and don't pass `--test-threads=1`. Every test has its own BoxLite home, so the default thread count is safe.

## `scripts/real-box`

```sh
scripts/real-box [libtest args]   # e.g. scripts/real-box egress:: auth::
```

It builds the tests, then runs `cargo test --locked --test real_box -- --ignored [libtest args]` at the default thread count. The arguments are test filters such as `egress::`, or `--skip` and `--exact`. The build and the tests share one time limit of 540 s (`REAL_BOX_TIME_LIMIT` seconds). The output also goes to `target/real-box/<timestamp>.log`.

The last line, starting `real-box:`, says what happened. The exit status is 0 if every test passed. It is non-zero if the build failed, a test failed or the time limit was hit (124). After the time limit, the script kills the VMs that stopped tests left running and removes their homes. A cold build can use up the time limit. If it does, run the script again: the build carries on from where it stopped.

## What to run

Run the groups your change touches. Each group is one module in `tests/real_box/`, and each module's doc comment names the `src/` files it covers. Filter by `<group>::`:

| group | covers |
| --- | --- |
| `lifecycle::` | `src/sandbox.rs`, `src/runtime.rs`, `src/reap.rs` |
| `mounts::` | `src/workspace.rs` and the mounts in `src/sandbox.rs` |
| `spec::` | `src/spec.rs` |
| `egress::` | `src/egress.rs` |
| `auth::` | `src/auth.rs` |
| `claude_config::` | `src/claude_config.rs` |
| `session::` | `src/session.rs`, `boxq shell` |
| `claude::` | `src/claude.rs`, `boxq run` |
| `setup::` | `src/setup.rs` |
| `base_image::` | `image/`, `spec::BASE_IMAGE` (see below) |

When you change the CLI, also run `cargo test --locked --test cli -- --ignored`, which takes a few seconds.

## The Base Image tests

Skip `base_image::` locally unless your change touches the image (`image/`, `spec::BASE_IMAGE`) or how Claude Code is launched (`src/claude.rs`, `src/claude_config.rs`). Each of its two tests takes about 3 minutes, mostly re-downloading the 581 MB Base Image. Without them the whole suite took 30–70 s on a 16-core host: `scripts/real-box --skip base_image::`. When you do need them, `scripts/real-box base_image::` runs them alone.

## CI

CI's `real-box` job runs the full suite, the Base Image tests included, on every push.
