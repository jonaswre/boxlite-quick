//! PROTOTYPE, throwaway. Answers wayfinder ticket
//! "How does boxq live with BoxLite's one-runtime-per-home lock?" (jonaswre/boxlite-quick#8).
//! Not boxq. No tests, minimal error handling.
//!
//! Tests option (b): hold the BoxliteRuntime (and so the BOXLITE_HOME lock) only long
//! enough to find, create or start the Box, drop it, then exec through the Box's own
//! gRPC socket (`boxes/<id>/sockets/box.sock`) with no runtime open at all.
//!
//!   runtime-lock-spike up    [path]               create/start the Box, then release the runtime
//!   runtime-lock-spike exec  [path] [-t] -- cmd   lock-free exec (-t: interactive TTY)
//!   runtime-lock-spike ls                         brief runtime, list Boxes
//!   runtime-lock-spike hold  <secs>               hold the runtime (simulates the old `boxq run`)
//!   runtime-lock-spike lock                       is the home lock free right now?
//!   runtime-lock-spike stop|rm [path]
//!
//! Every runtime open waits (100 ms polls, 120 s cap) instead of failing on the lock.
//! Env: SPIKE_HOME (default ~/.boxq-spike-PROTOTYPE-wipe-me, shares the image cache
//! with the box-claude-spike), SPIKE_IMAGE (default node:22-bookworm).

use boxlite::runtime::options::{BoxOptions, BoxliteOptions, NetworkSpec, RootfsSpec, VolumeSpec};
use boxlite::{BoxCommand, BoxliteRuntime, GuestSession};
use boxlite_shared::BoxTransport;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn t(start: Instant, what: &str) {
    eprintln!("[spike pid={} +{:>6.2}s] {what}", std::process::id(), start.elapsed().as_secs_f64());
}

fn home() -> PathBuf {
    std::env::var("SPIKE_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap()).join(".boxq-spike-PROTOTYPE-wipe-me")
    })
}

fn workspace(arg: Option<&String>) -> PathBuf {
    let p = arg.map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap());
    std::fs::create_dir_all(&p).unwrap();
    std::fs::canonicalize(p).expect("workspace path")
}

fn box_name(ws: &Path) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ws.hash(&mut h);
    format!("lock-spike-{:016x}", h.finish())
}

/// Probe the same flock BoxLite takes, without opening a runtime.
fn lock_is_free(home: &Path) -> bool {
    let f = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(home.join(".lock")).unwrap();
    let free = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if free {
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
    }
    free
}

/// Open the runtime, waiting for the home lock instead of failing on it.
fn open_runtime(start: Instant) -> anyhow::Result<BoxliteRuntime> {
    let waited = Instant::now();
    let mut announced = false;
    loop {
        match BoxliteRuntime::new(BoxliteOptions { home_dir: home(), ..Default::default() }) {
            Ok(rt) => {
                t(start, &format!("runtime open (waited {:.2}s for the lock)", waited.elapsed().as_secs_f64()));
                return Ok(rt);
            }
            Err(e) if e.to_string().contains("Another BoxliteRuntime") && waited.elapsed() < Duration::from_secs(120) => {
                if !announced {
                    t(start, "home lock busy, waiting");
                    announced = true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn options(ws: &Path) -> BoxOptions {
    let ws_s = ws.to_string_lossy().to_string();
    BoxOptions {
        rootfs: RootfsSpec::Image(std::env::var("SPIKE_IMAGE").unwrap_or("node:22-bookworm".into())),
        volumes: vec![VolumeSpec::bind_mount(&ws_s, &ws_s)],
        network: NetworkSpec::Enabled { allow_net: vec!["registry.npmjs.org".into()] },
        cpus: Some(2),
        memory_mib: Some(1024),
        working_dir: Some(ws_s),
        entrypoint: Some(vec![]),
        cmd: Some(vec!["sleep".into(), "infinity".into()]),
        auto_delete: Some(0),
        detach: true,
        ..Default::default()
    }
}

/// What a lock-free exec needs, gathered while the runtime is briefly held.
struct Target {
    box_id: String,
    container_id: String,
}

/// The container id is pub(crate) in boxlite 0.11; it is the one directory name under
/// `boxes/<id>/shared/containers/`. Spike-only reliance on internals.
fn container_id(box_id: &str) -> anyhow::Result<String> {
    let dir = home().join("boxes").join(box_id).join("shared/containers");
    let mut ids: Vec<String> = std::fs::read_dir(&dir)?.map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
    anyhow::ensure!(ids.len() == 1, "expected one container in {}, got {ids:?}", dir.display());
    Ok(ids.pop().unwrap())
}

/// Brief hold: find (and in `up`, create) the Box, make sure it runs, then drop the runtime.
async fn prepare(start: Instant, ws: &Path, create: bool) -> anyhow::Result<Target> {
    let rt = open_runtime(start)?;
    let name = box_name(ws);
    let b = if create {
        let (b, created) = rt.get_or_create(options(ws), Some(name)).await?;
        t(start, if created { "box created" } else { "box found" });
        b
    } else {
        rt.get(&name).await?.ok_or_else(|| anyhow::anyhow!("no box for this path; run `up` first"))?
    };
    let info = b.info().await?;
    if format!("{:?}", info.status) != "Running" {
        b.start().await?;
        t(start, "box started");
    }
    let box_id = b.id().to_string();
    drop(b);
    drop(rt);
    t(start, &format!("runtime dropped, home lock free = {}", lock_is_free(&home())));
    Ok(Target { container_id: container_id(&box_id)?, box_id })
}

fn term_size() -> Option<(u32, u32)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
    (r == 0 && ws.ws_col > 0).then(|| (ws.ws_row as u32, ws.ws_col as u32))
}

/// Exec through the Box's socket with no runtime: the path option (b) depends on.
async fn exec_lock_free(start: Instant, ws: &Path, target: &Target, argv: &[String], tty: bool) -> anyhow::Result<i32> {
    let sock = home().join("boxes").join(&target.box_id).join("sockets/box.sock");
    let session = GuestSession::new(BoxTransport::Unix { socket_path: sock });
    let mut ei = session.execution().await?;
    let mut cmd = BoxCommand::new(&argv[0])
        .args(&argv[1..])
        .env("BOXLITE_EXECUTOR", format!("container={}", target.container_id))
        .working_dir(ws.to_string_lossy())
        .tty(tty);
    if tty {
        cmd = cmd.env("TERM", std::env::var("TERM").unwrap_or("xterm-256color".into()));
    }
    let token = CancellationToken::new();
    let c = ei.exec(cmd, token.clone()).await?;
    t(start, &format!("lock-free exec started (execution {}), home lock free = {}", c.execution_id, lock_is_free(&home())));

    let saved = if tty {
        let saved = nix::sys::termios::tcgetattr(std::io::stdin()).ok();
        if let Some(s) = &saved {
            let mut raw = s.clone();
            nix::sys::termios::cfmakeraw(&mut raw);
            nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, &raw)?;
        }
        if let Some((rows, cols)) = term_size() {
            let _ = ei.resize_tty(&c.execution_id, rows, cols, 0, 0).await;
        }
        let mut resize_ei = ei.clone();
        let id = c.execution_id.clone();
        tokio::spawn(async move {
            let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).unwrap();
            while winch.recv().await.is_some() {
                if let Some((rows, cols)) = term_size() {
                    let _ = resize_ei.resize_tty(&id, rows, cols, 0, 0).await;
                }
            }
        });
        if let Some(tx) = c.stdin_tx.clone() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let mut stdin = std::io::stdin();
                while let Ok(n) = stdin.read(&mut buf) {
                    if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
        }
        saved
    } else {
        None
    };
    drop(c.stdin_tx);

    let (mut out_rx, mut err_rx, mut res_rx) = (c.stdout_rx, c.stderr_rx, c.result_rx);
    let out = tokio::spawn(async move {
        while let Some(s) = out_rx.recv().await {
            let mut o = std::io::stdout();
            let _ = o.write_all(s.as_bytes());
            let _ = o.flush();
        }
    });
    let err = tokio::spawn(async move {
        while let Some(s) = err_rx.recv().await {
            let _ = std::io::stderr().write_all(s.as_bytes());
        }
    });
    let result = res_rx.recv().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), async { let _ = out.await; let _ = err.await; }).await;
    if let Some(s) = saved {
        let _ = nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, &s);
    }
    token.cancel();
    let code = match result {
        Some(r) => {
            if let Some(m) = r.error_message {
                eprintln!("[spike] exec error: {m}");
            }
            r.exit_code
        }
        None => anyhow::bail!("exec result stream closed without a result"),
    };
    t(start, &format!("exec exit={code}"));
    Ok(code)
}

async fn real_main() -> anyhow::Result<i32> {
    let start = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str).unwrap_or("up");
    match sub {
        "lock" => {
            println!("home lock free = {}", lock_is_free(&home()));
            Ok(0)
        }
        "hold" => {
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
            let _rt = open_runtime(start)?;
            t(start, &format!("holding the runtime for {secs}s"));
            tokio::time::sleep(Duration::from_secs(secs)).await;
            Ok(0)
        }
        "ls" => {
            let rt = open_runtime(start)?;
            for i in rt.list_info().await? {
                println!("{} {:<40} {:?}", i.id, i.name.unwrap_or_default(), i.status);
            }
            t(start, "listed");
            Ok(0)
        }
        "up" => {
            let ws = workspace(args.get(2));
            let target = prepare(start, &ws, true).await?;
            println!("box={} container={}", target.box_id, target.container_id);
            Ok(0)
        }
        "exec" => {
            let ws = workspace(args.get(2).filter(|a| *a != "--" && *a != "-t"));
            let tty = args.iter().take_while(|a| *a != "--").any(|a| a == "-t");
            let argv: Vec<String> = args.iter().skip_while(|a| *a != "--").skip(1).cloned().collect();
            anyhow::ensure!(!argv.is_empty(), "usage: exec [path] [-t] -- cmd...");
            let target = prepare(start, &ws, false).await?;
            exec_lock_free(start, &ws, &target, &argv, tty).await
        }
        // Control: the stock path, runtime (and lock) held for the whole exec.
        "exec-held" => {
            let ws = workspace(args.get(2).filter(|a| *a != "--"));
            let argv: Vec<String> = args.iter().skip_while(|a| *a != "--").skip(1).cloned().collect();
            let rt = open_runtime(start)?;
            let b = rt.get(&box_name(&ws)).await?.ok_or_else(|| anyhow::anyhow!("no box"))?;
            let mut ex = b.exec(BoxCommand::new(&argv[0]).args(&argv[1..])).await?;
            t(start, &format!("held exec started, home lock free = {}", lock_is_free(&home())));
            let r = ex.wait().await?;
            Ok(r.exit_code)
        }
        "stop" | "rm" => {
            let ws = workspace(args.get(2));
            let rt = open_runtime(start)?;
            let name = box_name(&ws);
            if let Some(b) = rt.get(&name).await? {
                b.stop().await?;
                t(start, "stopped");
            }
            if sub == "rm" {
                rt.remove(&name, true).await?;
                t(start, "removed");
            }
            Ok(0)
        }
        other => anyhow::bail!("unknown subcommand {other}"),
    }
}

fn main() {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let code = match rt.block_on(real_main()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[spike] error: {e:#}");
            1
        }
    };
    drop(rt);
    std::process::exit(code);
}
