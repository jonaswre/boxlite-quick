use boxlite::runtime::options::BoxOptions;
use boxq::Workspace;
use boxq::auth::Auth;
use boxq::config::{BaseUrl, Config};

const KEY: &str = "sk-real-key-0123456789";

fn auth(base_url: &str) -> Auth {
    Auth::new(&BaseUrl::parse(base_url).unwrap(), KEY)
}

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
    let auth = auth("https://cliproxy.tailnet.ts.net/api");

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
    let auth = auth("https://cliproxy.tailnet.ts.net");

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
    let auth = auth("https://cliproxy.tailnet.ts.net");

    assert!(!format!("{auth:?}").contains(KEY));
}
