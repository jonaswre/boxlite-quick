use boxlite::runtime::options::BoxOptions;
use boxq::Workspace;
use boxq::auth::Auth;
use boxq::config::Config;

const KEY: &str = "sk-real-key-0123456789";

/// The Box options `auth` gives a Sandbox.
fn applied(auth: &Auth) -> BoxOptions {
    let mut options = BoxOptions::default();
    auth.apply(&mut options);
    options
}

fn env<'a>(options: &'a BoxOptions, key: &str) -> Option<&'a str> {
    options
        .env
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

#[test]
fn the_key_is_a_secret_scoped_to_the_base_url_host_and_the_guest_gets_its_placeholder() {
    let auth = Auth::new("https://cliproxy.tailnet.ts.net/api", KEY).unwrap();

    let options = applied(&auth);

    assert_eq!(options.secrets.len(), 1);
    let secret = &options.secrets[0];
    assert_eq!(secret.hosts, ["cliproxy.tailnet.ts.net"]);
    assert_eq!(secret.value, KEY);
    assert_eq!(
        env(&options, "ANTHROPIC_AUTH_TOKEN"),
        Some(secret.placeholder.as_str())
    );
    assert_eq!(
        env(&options, "ANTHROPIC_BASE_URL"),
        Some("https://cliproxy.tailnet.ts.net/api")
    );
}

#[test]
fn the_guest_env_never_holds_the_key_and_turns_off_claude_codes_nonessential_traffic() {
    let auth = Auth::new("https://cliproxy.tailnet.ts.net", KEY).unwrap();

    let options = applied(&auth);

    assert!(options.env.iter().all(|(_, v)| !v.contains(KEY)));
    assert_eq!(env(&options, "ANTHROPIC_API_KEY"), None);
    for key in [
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL",
        "DISABLE_UPDATES",
    ] {
        assert_eq!(env(&options, key), Some("1"), "{key}");
    }
}

#[test]
fn a_base_url_that_is_not_https_is_refused() {
    for url in ["http://cliproxy.lan", "cliproxy.lan", "ftp://cliproxy.lan"] {
        let err = Auth::new(url, KEY).unwrap_err();

        assert_eq!(
            err.to_string(),
            format!(
                "base URL {url:?} must start with https://: \
                 boxq can inject the API key only into HTTPS requests"
            )
        );
    }
}

#[test]
fn a_base_url_with_a_port_is_refused() {
    for url in ["https://cliproxy.lan:8317", "https://cliproxy.lan:443/v1"] {
        let err = Auth::new(url, KEY).unwrap_err();

        assert_eq!(
            err.to_string(),
            format!(
                "base URL {url:?} must not name a port: \
                 boxq can inject the API key only on HTTPS's default port"
            )
        );
    }
}

#[test]
fn a_base_url_must_name_a_plain_host() {
    for url in [
        "https://",
        "https:///v1",
        "https://[fd7a::1]/",
        "https://bad host/",
    ] {
        let err = Auth::new(url, KEY).unwrap_err();

        assert_eq!(
            err.to_string(),
            format!("base URL {url:?} must name a hostname or IPv4 address")
        );
    }
}

#[test]
fn a_base_url_carrying_credentials_is_refused_without_echoing_them() {
    let err = Auth::new("https://user:hunter2@cliproxy.lan/", KEY).unwrap_err();

    assert_eq!(
        err.to_string(),
        "the base URL must not carry credentials: put the API key in boxq's credentials file"
    );
}

/// The Config boxq loads with no config files and `env` as the only settings.
fn config(env: &[(&str, &str)]) -> Config {
    let home = tempfile::tempdir().unwrap();
    let ws = Workspace::resolve(home.path()).unwrap();
    let home_dir = home.path().to_string_lossy().into_owned();
    Config::load(&ws, |key| {
        let found = env
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string());
        found.or_else(|| (key == "HOME").then(|| home_dir.clone()))
    })
    .unwrap()
}

#[test]
fn auth_comes_from_the_merged_config() {
    let config = config(&[
        ("BOXQ_BASE_URL", "https://cliproxy.tailnet.ts.net"),
        ("BOXQ_API_KEY", KEY),
    ]);

    let options = applied(&Auth::from_config(&config).unwrap());

    assert_eq!(options.secrets[0].hosts, ["cliproxy.tailnet.ts.net"]);
    assert_eq!(options.secrets[0].value, KEY);
}

#[test]
fn auth_needs_both_a_base_url_and_an_api_key() {
    let no_url = config(&[("BOXQ_API_KEY", KEY)]);
    let no_key = config(&[("BOXQ_BASE_URL", "https://cliproxy.tailnet.ts.net")]);

    let no_url = Auth::from_config(&no_url).unwrap_err();
    let no_key = Auth::from_config(&no_key).unwrap_err();

    assert_eq!(
        no_url.to_string(),
        "no base URL is set: set `base_url` in the Global Config, or BOXQ_BASE_URL"
    );
    assert_eq!(
        no_key.to_string(),
        "no API key is set: set `api_key` in boxq's credentials file, or BOXQ_API_KEY"
    );
}

#[test]
fn auth_never_prints_the_key() {
    let auth = Auth::new("https://cliproxy.tailnet.ts.net", KEY).unwrap();

    assert!(!format!("{auth:?}").contains(KEY));
}
