# PROTOTYPE: runtime-lock-spike (throwaway, never merged)

Answers [How does boxq live with BoxLite's one-runtime-per-home lock?](https://github.com/jonaswre/boxlite-quick/issues/8).
This is not boxq; it is a single-file spike on `boxlite = "=0.11.0"`.

It tests option (b): open the `BoxliteRuntime` (and so take the `BOXLITE_HOME` flock) only long enough to find, create or start the Box, drop it, then exec through the Box's own gRPC socket with no runtime open.

- `GuestSession::new(BoxTransport::Unix { socket_path: <home>/boxes/<box-id>/sockets/box.sock })`, then `.execution().await?.exec(cmd, token)`.
- The command needs `BOXLITE_EXECUTOR=container=<container-id>` so it runs in the container rather than the guest root. The container id is `pub(crate)` in 0.11; the spike reads it from the single directory name under `<home>/boxes/<box-id>/shared/containers/`.
- Every runtime open polls the lock (100 ms, 120 s cap) instead of failing.
- `exec-held` is the control: stock `LiteBox::exec` with the runtime held for the whole exec.

## Run it

```sh
cd prototype/runtime-lock-spike
cargo build
S=./target/debug/runtime-lock-spike
$S up PROTOTYPE-workspace/a                  # create + start, then release the lock
$S exec PROTOTYPE-workspace/a -- sleep 40 &  # a long lock-free session
$S lock                                      # home lock free = true
$S ls                                        # works alongside
$S exec PROTOTYPE-workspace/a -- ps -ef      # "boxq shell" into the same Sandbox
$S up PROTOTYPE-workspace/b                  # a second Sandbox
$S exec PROTOTYPE-workspace/a -t -- bash     # interactive TTY
$S rm PROTOTYPE-workspace/a
```

Uses `~/.boxq-spike-PROTOTYPE-wipe-me` (override with `SPIKE_HOME`) so it shares the image cache with `box-claude-spike`. Host prerequisites are the same as that spike's.

Caution: on this host, `stop`/`rm` from a process other than the starter leaves the inner bwrap + `boxlite-shim` running (see the ticket). Check with `pgrep -af boxlite-shim` and kill leftovers.
