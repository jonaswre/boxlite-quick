use boxq::Workspace;
use boxq::config::{BaseUrl, Config};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// A fake host: a home directory, a Workspace, and the env boxq would see.
struct Host {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    workspace: Workspace,
    env: HashMap<&'static str, String>,
}

impl Host {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let ws = tmp.path().join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        let env = HashMap::from([("HOME", home.to_string_lossy().into_owned())]);
        Self {
            workspace: Workspace::resolve(&ws).unwrap(),
            _tmp: tmp,
            home,
            env,
        }
    }

    fn write_global(&self, toml: &str) {
        let dir = self.home.join(".config/boxq");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), toml).unwrap();
    }

    fn write_workspace(&self, toml: &str) -> PathBuf {
        let path = self.workspace.path().join(".boxq.toml");
        std::fs::write(&path, toml).unwrap();
        path
    }

    fn write_credentials(&self, toml: &str, mode: u32) -> PathBuf {
        let dir = self.home.join(".config/boxq");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials");
        std::fs::write(&path, toml).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    fn load(&self) -> anyhow::Result<Config> {
        Config::load(&self.workspace, |key| self.env.get(key).cloned())
    }
}

#[test]
fn with_no_config_files_boxq_uses_the_defaults() {
    let config = Host::new().load().unwrap();

    assert_eq!(config.base_url, None);
    assert!(config.api_key.is_none());
    assert_eq!(config.image, None);
    assert_eq!(config.setup, None);
    assert_eq!(config.resources.cpus, None);
    assert_eq!(config.resources.memory, None);
    assert_eq!(config.resources.disk, None);
    assert!(config.egress.builtins);
    assert!(config.egress.allow.is_empty());
    assert!(!config.egress.allow_host_loopback);
    assert!(config.egress.workspace_allow.is_empty());
}

#[test]
fn the_global_config_is_read_from_the_home_config_dir() {
    let host = Host::new();
    host.write_global(
        r#"
        base_url = "https://cliproxy.tailnet.ts.net"
        image = "ghcr.io/jonaswre/boxq:1"
        setup = "npm ci"

        [egress]
        builtins = false
        allow = ["*.mycorp.internal"]
        allow_host_loopback = true
        "#,
    );

    let config = host.load().unwrap();

    assert_eq!(
        config.base_url.as_ref().map(BaseUrl::as_str),
        Some("https://cliproxy.tailnet.ts.net")
    );
    assert_eq!(config.image.as_deref(), Some("ghcr.io/jonaswre/boxq:1"));
    assert_eq!(config.setup.as_deref(), Some("npm ci"));
    assert!(!config.egress.builtins);
    assert_eq!(config.egress.allow, ["*.mycorp.internal"]);
    assert!(config.egress.allow_host_loopback);
}

#[test]
fn xdg_config_home_replaces_the_home_config_dir() {
    let mut host = Host::new();
    host.write_global(r#"image = "from-home""#);
    let xdg = host.home.join("xdg");
    std::fs::create_dir_all(xdg.join("boxq")).unwrap();
    std::fs::write(xdg.join("boxq/config.toml"), r#"image = "from-xdg""#).unwrap();
    host.env
        .insert("XDG_CONFIG_HOME", xdg.to_string_lossy().into_owned());

    assert_eq!(host.load().unwrap().image.as_deref(), Some("from-xdg"));
}

#[test]
fn the_api_key_is_read_from_the_credentials_file() {
    let host = Host::new();
    host.write_credentials(r#"api_key = "sk-from-file""#, 0o600);

    let config = host.load().unwrap();

    assert_eq!(config.api_key.unwrap().expose(), "sk-from-file");
}

#[test]
fn a_group_readable_credentials_file_is_refused() {
    let host = Host::new();
    let path = host.write_credentials(r#"api_key = "sk-from-file""#, 0o640);

    let err = format!("{:#}", host.load().unwrap_err());

    assert!(err.contains(&*path.to_string_lossy()), "{err}");
    assert!(err.contains("chmod 600"), "{err}");
}

#[test]
fn a_world_readable_credentials_file_is_refused() {
    let host = Host::new();
    host.write_credentials(r#"api_key = "sk-from-file""#, 0o604);

    let err = format!("{:#}", host.load().unwrap_err());

    assert!(err.contains("chmod 600"), "{err}");
}

#[test]
fn the_api_key_never_appears_in_debug_output() {
    let host = Host::new();
    host.write_credentials(r#"api_key = "sk-secret""#, 0o600);

    let config = host.load().unwrap();

    assert!(!format!("{config:?}").contains("sk-secret"));
}

// Non-root users can't chown, so the credentials file is a symlink to a root-owned file.
#[test]
fn a_credentials_file_owned_by_another_user_is_refused() {
    use std::os::unix::fs::MetadataExt;
    let passwd = std::path::Path::new("/etc/passwd");
    assert_eq!(passwd.metadata().unwrap().uid(), 0);
    let host = Host::new();
    let dir = host.home.join(".config/boxq");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(passwd, dir.join("credentials")).unwrap();
    assert_ne!(
        dir.metadata().unwrap().uid(),
        0,
        "this test must not run as root"
    );

    let err = format!("{:#}", host.load().unwrap_err());

    assert!(err.contains("not owned by the current user"), "{err}");
}

#[test]
fn boxq_env_vars_override_the_files() {
    let mut host = Host::new();
    host.write_global(r#"base_url = "https://from-file""#);
    host.write_credentials(r#"api_key = "sk-from-file""#, 0o600);
    host.env.insert("BOXQ_BASE_URL", "https://from-env".into());
    host.env.insert("BOXQ_API_KEY", "sk-from-env".into());

    let config = host.load().unwrap();

    assert_eq!(
        config.base_url.as_ref().map(BaseUrl::as_str),
        Some("https://from-env")
    );
    assert_eq!(config.api_key.unwrap().expose(), "sk-from-env");
}

#[test]
fn anthropic_env_vars_are_ignored() {
    let mut host = Host::new();
    host.env
        .insert("ANTHROPIC_BASE_URL", "https://api.anthropic.com".into());
    host.env.insert("ANTHROPIC_API_KEY", "sk-ant".into());
    host.env.insert("ANTHROPIC_AUTH_TOKEN", "sk-ant".into());

    let config = host.load().unwrap();

    assert_eq!(config.base_url, None);
    assert!(config.api_key.is_none());
}

#[test]
fn the_workspace_config_wins_over_the_global_config_for_single_values() {
    let host = Host::new();
    host.write_global(
        r#"
        image = "global-image"
        setup = "global-setup"
        "#,
    );
    host.write_workspace(r#"image = "workspace-image""#);

    let config = host.load().unwrap();

    assert_eq!(config.image.as_deref(), Some("workspace-image"));
    assert_eq!(config.setup.as_deref(), Some("global-setup"));
}

#[test]
fn workspace_allow_entries_are_kept_apart_from_global_ones() {
    let host = Host::new();
    host.write_global(
        r#"
        [egress]
        allow = ["*.mycorp.internal"]
        "#,
    );
    host.write_workspace(
        r#"
        [egress]
        allow = ["registry.example.com"]
        "#,
    );

    let config = host.load().unwrap();

    assert_eq!(config.egress.allow, ["*.mycorp.internal"]);
    assert_eq!(config.egress.workspace_allow, ["registry.example.com"]);
}

#[test]
fn a_workspace_config_above_the_workspace_root_is_not_read() {
    let host = Host::new();
    let parent = host.workspace.path().parent().unwrap();
    std::fs::write(parent.join(".boxq.toml"), r#"image = "parent-image""#).unwrap();

    assert_eq!(host.load().unwrap().image, None);
}

#[test]
fn global_only_keys_in_the_workspace_config_are_errors_naming_file_and_key() {
    for (toml, key) in [
        (r#"base_url = "https://evil.example""#, "base_url"),
        ("[egress]\nbuiltins = false", "egress.builtins"),
        (
            "[egress]\nallow_host_loopback = true",
            "egress.allow_host_loopback",
        ),
    ] {
        let host = Host::new();
        let path = host.write_workspace(toml);

        let err = format!("{:#}", host.load().unwrap_err());

        assert!(err.contains(&*path.to_string_lossy()), "{err}");
        assert!(err.contains(&format!("`{key}`")), "{err}");
        assert!(err.contains("only in the Global Config"), "{err}");
    }
}

#[test]
fn unknown_keys_are_errors_naming_file_and_key() {
    let host = Host::new();
    host.write_global("[egress]\ndeny = [\"example.com\"]");
    let err = format!("{:#}", host.load().unwrap_err());
    assert!(err.contains("config.toml"), "{err}");
    assert!(err.contains("`deny`"), "{err}");

    let host = Host::new();
    let path = host.write_workspace("[network]\nmode = \"open\"");
    let err = format!("{:#}", host.load().unwrap_err());
    assert!(err.contains(&*path.to_string_lossy()), "{err}");
    assert!(err.contains("`network`"), "{err}");

    let host = Host::new();
    let path = host.write_credentials(r#"token = "sk-from-file""#, 0o600);
    let err = format!("{:#}", host.load().unwrap_err());
    assert!(err.contains(&*path.to_string_lossy()), "{err}");
    assert!(err.contains("`token`"), "{err}");
}

#[test]
fn errors_in_the_credentials_file_never_echo_its_contents() {
    for toml in [
        r#"token = "sk-secret""#,
        r#"api_key = "sk-secret"#,
        "api_key = sk-secret",
        "api_key = 123456789",
        "api_key = [\"sk-secret\"]",
    ] {
        let host = Host::new();
        host.write_credentials(toml, 0o600);

        let err = format!("{:#}", host.load().unwrap_err());

        assert!(!err.contains("sk-secret"), "{err}");
        assert!(!err.contains("123456789"), "{err}");
    }
}

#[test]
fn allow_entries_in_boxlites_grammar_are_accepted() {
    let host = Host::new();
    host.write_global(
        r#"
        [egress]
        allow = ["registry.npmjs.org", "*.mycorp.internal", "100.101.102.103", "10.0.0.0/8", "localhost"]
        "#,
    );

    assert_eq!(host.load().unwrap().egress.allow.len(), 5);
}

// BoxLite 0.11 silently drops a port (opening the host on every port) and silently
// ignores IPv6, so boxq refuses both rather than pass on a rule that lies.
#[test]
fn allow_entries_outside_boxlites_grammar_are_errors_naming_file_and_entry() {
    for entry in [
        "https://example.com",
        "example.com:8080",
        "10.0.0.1:443",
        "example.com/path",
        "::1",
        "fd00::/8",
        "10.0.0.0/33",
        "*.",
        "*",
        "foo.*.example.com",
        "exa mple.com",
        "",
    ] {
        let host = Host::new();
        let path = host.write_workspace(&format!("[egress]\nallow = [{entry:?}]"));

        let err = format!("{:#}", host.load().unwrap_err());

        assert!(err.contains(&*path.to_string_lossy()), "{entry}: {err}");
        assert!(err.contains(&format!("{entry:?}")), "{entry}: {err}");
    }
}

const OPENS_HOST_LOOPBACK: [&str; 5] = [
    "192.168.127.254",
    "192.168.0.0/16",
    "127.0.0.1",
    "127.0.0.0/8",
    "0.0.0.0/0",
];

#[test]
fn a_workspace_allow_entry_that_would_open_host_loopback_is_an_error() {
    for entry in OPENS_HOST_LOOPBACK {
        let host = Host::new();
        host.write_global("[egress]\nallow_host_loopback = true");
        let path = host.write_workspace(&format!("[egress]\nallow = [{entry:?}]"));

        let err = format!("{:#}", host.load().unwrap_err());

        assert!(err.contains(&*path.to_string_lossy()), "{entry}: {err}");
        assert!(err.contains(&format!("{entry:?}")), "{entry}: {err}");
        assert!(err.contains("host loopback"), "{entry}: {err}");
    }
}

#[test]
fn a_global_allow_entry_may_open_host_loopback_only_with_allow_host_loopback() {
    for entry in OPENS_HOST_LOOPBACK {
        let host = Host::new();
        host.write_global(&format!("[egress]\nallow = [{entry:?}]"));
        let err = format!("{:#}", host.load().unwrap_err());
        assert!(err.contains("config.toml"), "{entry}: {err}");
        assert!(err.contains("allow_host_loopback = true"), "{entry}: {err}");

        host.write_global(&format!(
            "[egress]\nallow = [{entry:?}]\nallow_host_loopback = true"
        ));
        assert!(host.load().is_ok(), "{entry}");
    }
}

#[test]
fn allow_entries_beside_host_loopback_are_accepted() {
    let host = Host::new();
    host.write_global(
        r#"[egress]
allow = ["192.168.127.253", "192.168.128.0/17", "0.0.0.0/2", "localhost"]"#,
    );
    host.write_workspace("[egress]\nallow = [\"10.0.0.0/8\"]");

    assert!(host.load().is_ok());
}

#[test]
fn global_allow_entries_are_validated_too() {
    let host = Host::new();
    host.write_global("[egress]\nallow = [\"example.com:22\"]");

    let err = format!("{:#}", host.load().unwrap_err());

    assert!(err.contains("config.toml"), "{err}");
    assert!(err.contains("\"example.com:22\""), "{err}");
}

fn resources(config: &Config) -> (Option<u8>, Option<u64>, Option<u64>) {
    let r = &config.resources;
    (
        r.cpus.map(|c| c.get()),
        r.memory.map(|s| s.mib()),
        r.disk.map(|s| s.mib()),
    )
}

#[test]
fn resources_take_cpus_and_iec_sizes() {
    let host = Host::new();
    host.write_global(
        r#"
        [resources]
        cpus = 4
        memory = "8GiB"
        disk = "512MiB"
        "#,
    );

    assert_eq!(
        resources(&host.load().unwrap()),
        (Some(4), Some(8192), Some(512))
    );
}

#[test]
fn workspace_resources_win_field_by_field() {
    let host = Host::new();
    host.write_global("[resources]\ncpus = 4\nmemory = \"8GiB\"");
    host.write_workspace("[resources]\ncpus = 2");

    assert_eq!(
        resources(&host.load().unwrap()),
        (Some(2), Some(8192), None)
    );
}

#[test]
fn sizes_outside_whole_mib_or_gib_are_errors_naming_file_and_value() {
    for size in [
        "\"8GB\"",
        "\"8G\"",
        "\"8gib\"",
        "\"8 GiB\"",
        "\"1.5GiB\"",
        "\"GiB\"",
        "\"-1GiB\"",
        "\"99999999999999999GiB\"",
        "8",
    ] {
        let host = Host::new();
        let path = host.write_workspace(&format!("[resources]\nmemory = {size}"));

        let err = format!("{:#}", host.load().unwrap_err());

        assert!(err.contains(&*path.to_string_lossy()), "{size}: {err}");
        assert!(err.contains(size), "{size}: {err}");
    }
}

#[test]
fn zero_cpus_is_an_error() {
    let host = Host::new();
    host.write_global("[resources]\ncpus = 0");

    let err = format!("{:#}", host.load().unwrap_err());

    assert!(err.contains("cpus"), "{err}");
}

#[test]
fn an_invalid_base_url_is_an_error() {
    let mut host = Host::new();
    host.env
        .insert("BOXQ_BASE_URL", "http://cliproxy.lan".into());

    let err = host.load().unwrap_err();

    assert!(
        err.to_string().contains("must start with https://"),
        "{err}"
    );
}

#[test]
fn a_base_url_names_its_host_lowercased() {
    let url = BaseUrl::parse("https://CliProxy.Tailnet.ts.net/api").unwrap();

    assert_eq!(url.as_str(), "https://CliProxy.Tailnet.ts.net/api");
    assert_eq!(url.host(), "cliproxy.tailnet.ts.net");
    assert_eq!(url.ipv4(), None);
    assert_eq!(
        BaseUrl::parse("https://10.0.0.5/").unwrap().ipv4(),
        Some([10, 0, 0, 5].into())
    );
}

#[test]
fn a_base_url_that_is_not_https_is_refused() {
    for url in ["http://cliproxy.lan", "cliproxy.lan", "ftp://cliproxy.lan"] {
        let err = BaseUrl::parse(url).unwrap_err();

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
        let err = BaseUrl::parse(url).unwrap_err();

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
        let err = BaseUrl::parse(url).unwrap_err();

        assert_eq!(
            err.to_string(),
            format!("base URL {url:?} must name a hostname or IPv4 address")
        );
    }
}

#[test]
fn a_base_url_carrying_credentials_is_refused_without_echoing_them() {
    let err = BaseUrl::parse("https://user:hunter2@cliproxy.lan/").unwrap_err();

    assert_eq!(
        err.to_string(),
        "the base URL must not carry credentials: put the API key in boxq's credentials file"
    );
}
