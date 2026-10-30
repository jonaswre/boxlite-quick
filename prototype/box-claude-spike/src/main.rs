//! PROTOTYPE, throwaway. Answers wayfinder ticket
//! "Can a Box run Claude Code end to end against CLIProxyAPI over Tailscale?"
//! (jonaswre/boxlite#5). Not boxq. No tests, minimal error handling.
//!
//!   box-claude-spike run  [path]            create/reattach, set up once, launch Claude Code
//!   box-claude-spike up   [path]            same as run, minus launching Claude Code
//!   box-claude-spike exec [path] -- cmd...  run a non-interactive command in the Box
//!   box-claude-spike stop|rm [path]
//!   box-claude-spike ls
//!
//! Env: ANTHROPIC_BASE_URL + ANTHROPIC_AUTH_TOKEN from the host.
//!      SPIKE_NO_JAILER=1 disables BoxLite's bwrap jailer.
//!      SPIKE_IMAGE overrides the image (default node:22-bookworm).

mod terminal;

use boxlite::runtime::options::{BoxOptions, BoxliteOptions, NetworkSpec, RootfsSpec, Secret, VolumeSpec};
use boxlite::{BoxCommand, BoxliteRuntime, LiteBox};
use std::hash::{Hash, Hasher};
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::time::Instant;

const CLAUDE_VERSION: &str = "2.1.312";
const PLACEHOLDER: &str = "<BOXLITE_SECRET:anthropic>";
const REGISTRY: &str = "registry.npmjs.org";

fn t(start: Instant, what: &str) {
    eprintln!("[spike +{:>6.2}s] {what}", start.elapsed().as_secs_f64());
}

fn workspace(arg: Option<&String>) -> PathBuf {
    let p = arg.map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap());
    std::fs::canonicalize(p).expect("workspace path")
}

fn box_name(ws: &PathBuf) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ws.hash(&mut h);
    format!("boxq-spike-{:016x}", h.finish())
}

fn base_url_host() -> String {
    let url = std::env::var("ANTHROPIC_BASE_URL").expect("ANTHROPIC_BASE_URL");
    let rest = url.strip_prefix("https://").expect("base URL must be https://");
    let host = rest.split('/').next().unwrap();
    assert!(!host.contains(':'), "base URL must not carry a port (secret injection is 443-only)");
    host.to_string()
}

fn options(ws: &PathBuf) -> BoxOptions {
    let host = base_url_host();
    // CGNAT tailnet IPs are refused on a hostname rule alone, so also allow the exact IP.
    let ip = (host.as_str(), 443).to_socket_addrs().unwrap().next().unwrap().ip().to_string();
    let allow_net = vec![host.clone(), ip, REGISTRY.to_string()];
    eprintln!("[spike] allow_net = {allow_net:?}");

    let ws_s = ws.to_string_lossy().to_string();
    let mut o = BoxOptions {
        rootfs: RootfsSpec::Image(std::env::var("SPIKE_IMAGE").unwrap_or("node:22-bookworm".into())),
        volumes: vec![VolumeSpec::bind_mount(&ws_s, &ws_s)],
        network: NetworkSpec::Enabled { allow_net },
        secrets: vec![Secret {
            name: "anthropic".into(),
            hosts: vec![host],
            placeholder: PLACEHOLDER.into(),
            value: std::env::var("ANTHROPIC_AUTH_TOKEN").expect("ANTHROPIC_AUTH_TOKEN"),
        }],
        env: vec![
            ("ANTHROPIC_BASE_URL".into(), std::env::var("ANTHROPIC_BASE_URL").unwrap()),
            ("ANTHROPIC_AUTH_TOKEN".into(), PLACEHOLDER.into()),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into()),
            ("CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL".into(), "1".into()),
            ("DISABLE_UPDATES".into(), "1".into()),
            ("NODE_EXTRA_CA_CERTS".into(), "/etc/ssl/certs/ca-certificates.crt".into()),
            ("IS_SANDBOX".into(), "1".into()),
        ],
        cpus: Some(4),
        memory_mib: Some(4096),
        working_dir: Some(ws_s),
        entrypoint: Some(vec![]),
        cmd: Some(vec!["sleep".into(), "infinity".into()]),
        auto_delete: Some(0),
        detach: true,
        ..Default::default()
    };
    if std::env::var("SPIKE_NO_JAILER").is_ok() {
        o.advanced.security.jailer_enabled = false;
    }
    o
}

fn setup_script(ws: &PathBuf) -> String {
    let ws = format!("'{}'", ws.to_string_lossy().replace('\'', "'\"'\"'"));
    format!(
        r#"set -e
if ! test -f /opt/boxq-setup-done; then
  echo "setup: installing Claude Code {CLAUDE_VERSION} from npm"
  npm install -g --no-fund --no-audit @anthropic-ai/claude-code@{CLAUDE_VERSION}
else
  echo "setup: already installed"
fi
mkdir -p /root/.claude
# Reconcile configuration on every attachment, preserving unrelated state.
node - {ws} <<'NODE'
const fs = require('fs');
const workspace = process.argv[2];
function update(path, apply) {{
  const value = fs.existsSync(path) ? JSON.parse(fs.readFileSync(path, 'utf8')) : {{}};
  apply(value);
  const temporary = path + '.boxq-' + process.pid;
  fs.writeFileSync(temporary, JSON.stringify(value, null, 2) + '\n', {{mode: 0o600}});
  fs.renameSync(temporary, path);
}}
update('/root/.claude.json', c => {{
  c.hasCompletedOnboarding = true;
  c.projects ??= {{}};
  c.projects[workspace] ??= {{}};
  c.projects[workspace].hasTrustDialogAccepted = true;
}});
update('/root/.claude/settings.json', c => {{
  c.disableAutoMode = 'disable';
  c.skipDangerousModePermissionPrompt = true;
  c.skipWebFetchPreflight = true;
  c.permissions = {{...c.permissions, defaultMode: 'bypassPermissions'}};
}});
NODE
touch /opt/boxq-setup-done
echo "setup: configuration applied"
"#
    )
}

async fn run_cmd(b: &LiteBox, cmd: BoxCommand, interactive: bool, tty: bool) -> anyhow::Result<i32> {
    let mut ex = b.exec(cmd).await?;
    if tty {
        if let Some((cols, rows)) = term_size() {
            let _ = ex.resize_tty(rows, cols).await;
        }
    }
    terminal::StreamManager::new(&mut ex, interactive, tty).start().await
}

fn term_size() -> Option<(u32, u32)> {
    let mut ws: nix::libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { nix::libc::ioctl(0, nix::libc::TIOCGWINSZ, &mut ws) };
    (r == 0 && ws.ws_col > 0).then(|| (ws.ws_col as u32, ws.ws_row as u32))
}

async fn real_main() -> anyhow::Result<i32> {
    let start = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str).unwrap_or("run");
    let home = PathBuf::from(std::env::var("HOME")?).join(".boxq-spike-PROTOTYPE-wipe-me");
    let rt = BoxliteRuntime::new(BoxliteOptions { home_dir: home, ..Default::default() })?;
    t(start, "runtime open");

    if sub == "ls" {
        for i in rt.list_info().await? {
            println!("{:?}", i);
        }
        return Ok(0);
    }

    let path_arg = args.get(2).filter(|a| *a != "--");
    let ws = workspace(path_arg);
    let name = box_name(&ws);
    eprintln!("[spike] workspace={} box={name}", ws.display());

    match sub {
        "stop" | "rm" => {
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
        "exec" => {
            let rest: Vec<String> = args.iter().skip_while(|a| *a != "--").skip(1).cloned().collect();
            let b = rt.get(&name).await?.ok_or_else(|| anyhow::anyhow!("no box; run first"))?;
            let code = run_cmd(&b, BoxCommand::new(&rest[0]).args(&rest[1..]), false, false).await?;
            t(start, &format!("exec exit={code}"));
            Ok(code)
        }
        "run" | "up" => {
            let (b, created) = rt.get_or_create(options(&ws), Some(name)).await?;
            t(start, if created { "box created" } else { "box found (reattach)" });
            b.start().await?;
            t(start, "box started");
            let code = run_cmd(&b, BoxCommand::new("sh").args(["-c", &setup_script(&ws)]), false, false).await?;
            t(start, &format!("setup exit={code}"));
            if code != 0 || sub == "up" {
                return Ok(code);
            }
            let claude = BoxCommand::new("claude")
                .arg("--dangerously-skip-permissions")
                .env("TERM", std::env::var("TERM").unwrap_or("xterm-256color".into()))
                .working_dir(ws.to_string_lossy())
                .tty(true);
            t(start, "launching claude");
            let code = run_cmd(&b, claude, true, true).await?;
            t(start, &format!("claude exit={code}"));
            Ok(code)
        }
        other => anyhow::bail!("unknown subcommand {other}"),
    }
}

fn main() {
    tracing_subscriber_init();
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

fn tracing_subscriber_init() {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    }
}
