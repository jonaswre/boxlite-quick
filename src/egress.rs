//! The Egress Policy: which outbound destinations a Sandbox may reach (jonaswre/boxlite-quick#13).

use crate::Workspace;
use crate::config::{Config, Egress};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use url::{Host, Url};

/// What the Agent needs whatever the Global Config says. `registry.npmjs.org` serves the
/// interim install of Claude Code (`crate::claude`), and goes once the base image ships it.
const AGENT: &[&str] = &["api.anthropic.com", "registry.npmjs.org"];

/// The forge and registries a Sandbox reaches unless the Global Config sets `builtins = false`.
/// With its nonessential traffic off, Claude Code itself needs only `registry.npmjs.org` (`npx`
/// MCP servers) and `github.com` (plugin marketplaces) from these (jonaswre/boxlite-quick#3).
const BUILTINS: &[&str] = &[
    "github.com",
    "*.github.com",
    "*.githubusercontent.com",
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "*.crates.io",
];

/// The guest's address for the host's 127.0.0.1 (`host.boxlite.internal`).
const HOST_LOOPBACK: Ipv4Addr = Ipv4Addr::new(192, 168, 127, 254);

/// A Sandbox's `allow_net`. Never empty, since BoxLite reads an empty list as full access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    allow_net: Vec<String>,
}

impl Policy {
    /// The Workspace's policy in boxq's real environment, asking on the terminal for an Egress
    /// Approval whenever the Workspace Config's `allow` entries have changed.
    pub fn from_process_env(ws: &Workspace, config: &Config) -> anyhow::Result<Self> {
        let approved = Approvals::from_process_env()?.ask_if_changed(
            ws,
            &config.egress.workspace_allow,
            ask_on_terminal,
        )?;
        Self::new(
            &config.egress,
            config.base_url.as_deref(),
            approved,
            resolve,
        )
    }

    /// The policy from `egress` and the base URL, with the Workspace Config's entries only if
    /// `workspace_approved`. `resolve` gives a hostname's IPv4 addresses on the host.
    pub fn new(
        egress: &Egress,
        base_url: Option<&str>,
        workspace_approved: bool,
        resolve: impl Fn(&str) -> Vec<Ipv4Addr>,
    ) -> anyhow::Result<Self> {
        let mut allow_net: Vec<String> = AGENT.iter().map(|&e| e.into()).collect();
        if egress.builtins {
            allow_net.extend(BUILTINS.iter().map(|&e| e.into()));
        }
        let workspace = egress.workspace_allow.iter().filter(|_| workspace_approved);
        for entry in egress.allow.iter().chain(workspace) {
            // A hostname rule alone is refused when the name resolves into CGNAT space, as a
            // tailnet's does, so the exact IP goes next to it.
            if is_hostname(entry) {
                let tailnet = resolve(entry).into_iter().filter(|&ip| in_tailnet(ip));
                allow_net.extend(tailnet.map(|ip| ip.to_string()));
            }
            allow_net.push(entry.clone());
        }
        if let Some(url) = base_url {
            let ips = match base_url_host(url)? {
                Host::Domain(name) => {
                    let ips = resolve(&name);
                    if ips.is_empty() {
                        eprintln!(
                            "warning: cannot resolve the base URL's host {name}; allowing it by \
                             name only"
                        );
                    }
                    allow_net.push(name);
                    ips
                }
                Host::Ipv4(ip) => vec![ip],
                Host::Ipv6(_) => anyhow::bail!(
                    "invalid base URL {url:?}: the Egress Policy cannot allow an IPv6 address"
                ),
            };
            // An IP rule for host loopback would open it to any hostname that resolves there.
            let ips = ips.into_iter().filter(|&ip| !opens_host_loopback(ip, 32));
            allow_net.extend(ips.map(|ip| ip.to_string()));
        }
        if egress.allow_host_loopback {
            allow_net.push(HOST_LOOPBACK.to_string());
        }
        allow_net.sort();
        allow_net.dedup();
        Ok(Self { allow_net })
    }

    /// The sorted, deduplicated entries.
    pub fn allow_net(&self) -> &[String] {
        &self.allow_net
    }
}

fn base_url_host(url: &str) -> anyhow::Result<Host> {
    Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .and_then(|u| u.host().map(|h| h.to_owned()))
        .ok_or_else(|| anyhow::anyhow!("invalid base URL {url:?}: use an http:// or https:// URL"))
}

/// The Egress Approvals: for each Workspace, a hash of the Workspace Config `allow` entries the
/// user consented to.
pub struct Approvals {
    path: PathBuf,
}

/// The approvals file as written.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalsFile {
    #[serde(default)]
    workspaces: BTreeMap<String, String>,
}

impl Approvals {
    pub fn new(path: &Path) -> Self {
        Self { path: path.into() }
    }

    /// The approvals in boxq's real environment.
    pub fn from_process_env() -> anyhow::Result<Self> {
        Self::from_env(|key| std::env::var(key).ok())
    }

    /// The approvals at `$XDG_STATE_HOME/boxq/approvals.toml`, falling back to
    /// `~/.local/state/boxq/approvals.toml`, reading the environment through `env`.
    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let base = match env("XDG_STATE_HOME").filter(|dir| Path::new(dir).is_absolute()) {
            Some(dir) => PathBuf::from(dir),
            None => {
                let home = env("HOME").context("cannot find boxq's state: HOME is not set")?;
                Path::new(&home).join(".local/state")
            }
        };
        Ok(Self::new(&base.join("boxq/approvals.toml")))
    }

    /// Whether the Workspace's `entries` apply. Unless they are the ones last approved, `ask` is
    /// shown them sorted, and its yes is recorded.
    pub fn ask_if_changed(
        &self,
        ws: &Workspace,
        entries: &[String],
        ask: impl FnOnce(&[String]) -> anyhow::Result<bool>,
    ) -> anyhow::Result<bool> {
        if entries.is_empty() {
            return Ok(true);
        }
        let mut sorted = entries.to_vec();
        sorted.sort();
        sorted.dedup();
        let hash = hash(&sorted);
        let mut file = self.read()?;
        let key = key(ws);
        if file.workspaces.get(&key) == Some(&hash) {
            return Ok(true);
        }
        if !ask(&sorted)? {
            return Ok(false);
        }
        file.workspaces.insert(key, hash);
        self.write(&file)?;
        Ok(true)
    }

    /// Forgets the Workspace's approval, so its entries are asked about again.
    pub fn revoke(&self, ws: &Workspace) -> anyhow::Result<()> {
        let mut file = self.read()?;
        if file.workspaces.remove(&key(ws)).is_some() {
            self.write(&file)?;
        }
        Ok(())
    }

    fn read(&self) -> anyhow::Result<ApprovalsFile> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("invalid {}", self.path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ApprovalsFile::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", self.path.display())),
        }
    }

    fn write(&self, file: &ApprovalsFile) -> anyhow::Result<()> {
        let toml = toml::to_string(file)
            .with_context(|| format!("cannot write {}", self.path.display()))?;
        let dir = self.path.parent().unwrap();
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        // Renamed into place, so the file is never read half-written.
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("cannot write {}", self.path.display()))
    }
}

fn key(ws: &Workspace) -> String {
    ws.path().to_string_lossy().into_owned()
}

/// The SHA-256 of sorted `entries`, one per line, in hex.
fn hash(entries: &[String]) -> String {
    let digest = Sha256::digest(entries.join("\n"));
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lists `entries` and asks y/N on the terminal. With no terminal to ask on, the answer is no.
fn ask_on_terminal(entries: &[String]) -> anyhow::Result<bool> {
    let listed = entries.join(", ");
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        eprintln!(
            "warning: running without the Workspace Config's egress entries ({listed}): they \
             have no Egress Approval, and there is no terminal to ask for one"
        );
        return Ok(false);
    }
    eprintln!("This Workspace's .boxq.toml allows outbound traffic to:");
    for entry in entries {
        eprintln!("  {entry}");
    }
    eprint!("Allow it? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    let yes = matches!(answer.trim(), "y" | "Y" | "yes");
    if !yes {
        eprintln!("warning: running without the Workspace Config's egress entries ({listed})");
    }
    Ok(yes)
}

/// The host's IPv4 addresses for `host`, or none if it doesn't resolve.
fn resolve(host: &str) -> Vec<Ipv4Addr> {
    let Ok(addrs) = (host, 0).to_socket_addrs() else {
        return vec![];
    };
    addrs
        .filter_map(|addr| match addr {
            SocketAddr::V4(addr) => Some(*addr.ip()),
            SocketAddr::V6(_) => None,
        })
        .collect()
}

/// Whether `entry` is an exact hostname rule, not a wildcard, IP or CIDR.
fn is_hostname(entry: &str) -> bool {
    !entry.starts_with("*.") && !entry.contains('/') && entry.parse::<Ipv4Addr>().is_err()
}

/// Whether `ip` is in 100.64.0.0/10, the CGNAT range tailnets use.
fn in_tailnet(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && b & 0xc0 == 64
}

/// Whether the IP or CIDR rule `entry` would open host loopback. Hostnames never do: BoxLite
/// refuses a name that resolves to loopback unless an IP rule covers it.
pub(crate) fn entry_opens_host_loopback(entry: &str) -> bool {
    let (ip, prefix) = entry.split_once('/').unwrap_or((entry, "32"));
    match (ip.parse(), prefix.parse()) {
        (Ok(ip), Ok(prefix)) => opens_host_loopback(ip, prefix),
        _ => false,
    }
}

/// Whether an IP rule for `ip/prefix` would open host loopback: it covers [`HOST_LOOPBACK`], or
/// overlaps 127.0.0.0/8, which a hostname rule that resolves there would then reach.
fn opens_host_loopback(ip: Ipv4Addr, prefix: u8) -> bool {
    let overlaps = |net: Ipv4Addr, bits: u8| {
        let bits = prefix.min(bits);
        let mask = u32::MAX.checked_shl(32 - u32::from(bits)).unwrap_or(0);
        u32::from(ip) & mask == u32::from(net) & mask
    };
    overlaps(HOST_LOOPBACK, 32) || overlaps(Ipv4Addr::LOCALHOST, 8)
}
