//! The Agent's auth: the API key goes to BoxLite as a secret scoped to the base URL's host, so
//! the guest only ever holds a placeholder (jonaswre/boxlite-quick#14).

use crate::config::Config;
use anyhow::Context;
use boxlite::runtime::options::{BoxOptions, Secret};
use sha2::{Digest, Sha256};

/// What the guest holds in place of the API key. BoxLite swaps it for the key in HTTPS
/// requests to the base URL's host.
const PLACEHOLDER: &str = "<BOXLITE_SECRET:anthropic>";

/// The base URL and API key a Sandbox authenticates with. Its `Debug` output redacts the key.
#[derive(Debug)]
pub struct Auth {
    base_url: String,
    secret: Secret,
}

impl Auth {
    /// The merged config's base URL and API key.
    pub fn from_config(config: &Config) -> anyhow::Result<Self> {
        let base_url = config
            .base_url
            .as_deref()
            .context("no base URL is set: set `base_url` in the Global Config, or BOXQ_BASE_URL")?;
        let api_key = config.api_key.as_ref().context(
            "no API key is set: set `api_key` in boxq's credentials file, or BOXQ_API_KEY",
        )?;
        Self::new(base_url, api_key.expose())
    }

    /// Refuses a base URL BoxLite can't inject the key into: anything but `https://` on the
    /// default port.
    pub fn new(base_url: &str, api_key: &str) -> anyhow::Result<Self> {
        let Some(rest) = base_url.strip_prefix("https://") else {
            anyhow::bail!(
                "base URL {base_url:?} must start with https://: \
                 boxq can inject the API key only into HTTPS requests"
            );
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        // Not echoed, since the URL would print the credentials.
        anyhow::ensure!(
            !authority.contains('@'),
            "the base URL must not carry credentials: put the API key in boxq's credentials file"
        );
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        anyhow::ensure!(
            !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
            "base URL {base_url:?} must name a hostname or IPv4 address"
        );
        // BoxLite 0.11 injects secrets only on port 443, and how to reach any other port is
        // still undecided (the map's "Plain-HTTP or non-443 base URLs").
        anyhow::ensure!(
            port.is_none(),
            "base URL {base_url:?} must not name a port: \
             boxq can inject the API key only on HTTPS's default port"
        );
        Ok(Self {
            base_url: base_url.into(),
            secret: Secret {
                name: "anthropic".into(),
                hosts: vec![host.to_ascii_lowercase()],
                placeholder: PLACEHOLDER.into(),
                value: api_key.into(),
            },
        })
    }

    /// Gives a Box created with `options` the key as a secret, and the guest the env that
    /// points Claude Code at the base URL with the placeholder.
    pub fn apply(&self, options: &mut BoxOptions) {
        options.secrets.push(self.secret.clone());
        options.env.extend([
            ("ANTHROPIC_BASE_URL".into(), self.base_url.clone()),
            ("ANTHROPIC_AUTH_TOKEN".into(), PLACEHOLDER.into()),
            // Updates, telemetry, error reports and the plugin marketplace would each need
            // their own egress, and the Agent works without them (jonaswre/boxlite#3).
            (
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
                "1".into(),
            ),
            (
                "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL".into(),
                "1".into(),
            ),
            ("DISABLE_UPDATES".into(), "1".into()),
        ]);
    }

    /// A digest that tells two Auths apart without holding the key. BoxLite's own database,
    /// in the same home, already holds the key itself, so this exposes nothing new.
    pub(crate) fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.base_url.as_bytes());
        digest.update([0]);
        digest.update(self.secret.value.as_bytes());
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}
