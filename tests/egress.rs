use boxq::Workspace;
use boxq::config::Egress;
use boxq::egress::{Approvals, Policy};
use std::net::Ipv4Addr;

/// The Egress settings with no config files.
fn defaults() -> Egress {
    Egress {
        builtins: true,
        allow: vec![],
        allow_host_loopback: false,
        workspace_allow: vec![],
    }
}

/// A resolver that knows no names.
fn no_dns(_: &str) -> Vec<Ipv4Addr> {
    vec![]
}

fn allow_net(policy: Policy) -> Vec<String> {
    policy.allow_net().to_vec()
}

#[test]
fn by_default_the_policy_is_the_built_in_allowlist() {
    let policy = Policy::new(&defaults(), None, false, no_dns).unwrap();

    assert_eq!(
        allow_net(policy),
        [
            "*.crates.io",
            "*.github.com",
            "*.githubusercontent.com",
            "api.anthropic.com",
            "crates.io",
            "files.pythonhosted.org",
            "github.com",
            "pypi.org",
            "registry.npmjs.org",
        ]
    );
}

#[test]
fn turning_the_builtins_off_keeps_the_anthropic_entries() {
    let egress = Egress {
        builtins: false,
        ..defaults()
    };

    let policy = Policy::new(&egress, None, false, no_dns).unwrap();

    assert_eq!(allow_net(policy), ["api.anthropic.com"]);
}

#[test]
fn global_entries_always_apply_and_workspace_entries_only_with_an_approval() {
    let egress = Egress {
        builtins: false,
        allow: vec!["*.mycorp.internal".into()],
        workspace_allow: vec!["example.org".into()],
        ..defaults()
    };

    let unapproved = Policy::new(&egress, None, false, no_dns).unwrap();
    let approved = Policy::new(&egress, None, true, no_dns).unwrap();

    assert_eq!(
        allow_net(unapproved),
        ["*.mycorp.internal", "api.anthropic.com"]
    );
    assert_eq!(
        allow_net(approved),
        ["*.mycorp.internal", "api.anthropic.com", "example.org"]
    );
}

/// A resolver that knows one name.
fn dns(name: &'static str, ips: &'static [[u8; 4]]) -> impl Fn(&str) -> Vec<Ipv4Addr> {
    move |host| match host == name {
        true => ips.iter().map(|&ip| ip.into()).collect(),
        false => vec![],
    }
}

#[test]
fn the_base_urls_host_and_its_resolved_ips_are_always_allowed() {
    let egress = Egress {
        builtins: false,
        ..defaults()
    };
    let resolve = dns(
        "cliproxy.example.net",
        &[[203, 0, 113, 7], [203, 0, 113, 8]],
    );

    let policy = Policy::new(
        &egress,
        Some("https://cliproxy.example.net/anthropic"),
        false,
        resolve,
    )
    .unwrap();

    assert_eq!(
        allow_net(policy),
        [
            "203.0.113.7",
            "203.0.113.8",
            "api.anthropic.com",
            "cliproxy.example.net",
        ]
    );
}

#[test]
fn a_base_url_with_an_ip_host_allows_that_ip() {
    let egress = Egress {
        builtins: false,
        ..defaults()
    };

    let policy = Policy::new(&egress, Some("http://10.0.0.5:8317"), false, no_dns).unwrap();

    assert_eq!(allow_net(policy), ["10.0.0.5", "api.anthropic.com"]);
}

#[test]
fn a_base_url_without_a_host_is_refused() {
    let err = Policy::new(&defaults(), Some("cliproxy.example.net"), false, no_dns).unwrap_err();

    assert_eq!(
        err.to_string(),
        "invalid base URL \"cliproxy.example.net\": use an http:// or https:// URL"
    );
}

#[test]
fn an_allowed_hostname_in_tailnet_space_also_allows_its_ip() {
    let egress = Egress {
        builtins: false,
        allow: vec!["nas.tailnet.ts.net".into(), "example.com".into()],
        ..defaults()
    };
    let resolve = |host: &str| match host {
        "nas.tailnet.ts.net" => vec![Ipv4Addr::new(100, 101, 102, 103)],
        "example.com" => vec![Ipv4Addr::new(93, 184, 215, 14)],
        _ => vec![],
    };

    let policy = Policy::new(&egress, None, false, resolve).unwrap();

    assert_eq!(
        allow_net(policy),
        [
            "100.101.102.103",
            "api.anthropic.com",
            "example.com",
            "nas.tailnet.ts.net",
        ]
    );
}

#[test]
fn an_approved_workspace_hostname_in_tailnet_space_also_allows_its_ip() {
    let egress = Egress {
        builtins: false,
        workspace_allow: vec!["git.tailnet.ts.net".into()],
        ..defaults()
    };
    let resolve = dns("git.tailnet.ts.net", &[[100, 64, 0, 1]]);

    let policy = Policy::new(&egress, None, true, resolve).unwrap();

    assert_eq!(
        allow_net(policy),
        ["100.64.0.1", "api.anthropic.com", "git.tailnet.ts.net"]
    );
}

#[test]
fn host_loopback_is_allowed_only_when_the_global_config_opens_it() {
    let closed = defaults();
    let open = Egress {
        allow_host_loopback: true,
        ..defaults()
    };

    let closed = Policy::new(&closed, None, false, no_dns).unwrap();
    let open = Policy::new(&open, None, false, no_dns).unwrap();

    assert!(!closed.allow_net().contains(&"192.168.127.254".into()));
    assert!(open.allow_net().contains(&"192.168.127.254".into()));
}

#[test]
fn a_base_url_on_host_loopback_does_not_open_it() {
    let egress = Egress {
        builtins: false,
        ..defaults()
    };
    let resolve = dns("localhost", &[[127, 0, 0, 1]]);

    let by_name = Policy::new(&egress, Some("http://localhost:8317"), false, resolve).unwrap();
    let by_ip = Policy::new(&egress, Some("http://127.0.0.1:8317"), false, no_dns).unwrap();

    assert_eq!(allow_net(by_name), ["api.anthropic.com", "localhost"]);
    assert_eq!(allow_net(by_ip), ["api.anthropic.com"]);
}

/// A temporary state directory and two Workspaces.
struct State {
    tmp: tempfile::TempDir,
    approvals: Approvals,
    ws: Workspace,
    other: Workspace,
}

impl State {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let ws = |name| {
            let dir = tmp.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            Workspace::resolve(&dir).unwrap()
        };
        Self {
            approvals: Approvals::new(&tmp.path().join("state/boxq/approvals.toml")),
            ws: ws("project"),
            other: ws("other"),
            tmp,
        }
    }
}

fn entries(list: &[&str]) -> Vec<String> {
    list.iter().map(|&e| e.into()).collect()
}

fn yes(_: &[String]) -> anyhow::Result<bool> {
    Ok(true)
}

fn no(_: &[String]) -> anyhow::Result<bool> {
    Ok(false)
}

fn never_asked(listed: &[String]) -> anyhow::Result<bool> {
    panic!("asked about {listed:?}")
}

#[test]
fn workspace_entries_are_asked_about_once_then_applied() {
    let s = State::new();
    let allow = entries(&["example.org", "*.example.net"]);
    let mut listed = vec![];

    let first = s.approvals.ask_if_changed(&s.ws, &allow, |l| {
        listed = l.to_vec();
        Ok(true)
    });
    let second = s.approvals.ask_if_changed(&s.ws, &allow, never_asked);

    assert!(first.unwrap());
    assert_eq!(listed, ["*.example.net", "example.org"]);
    assert!(second.unwrap());
}

#[test]
fn reordered_workspace_entries_need_no_new_approval() {
    let s = State::new();
    s.approvals
        .ask_if_changed(&s.ws, &entries(&["a.example", "b.example"]), yes)
        .unwrap();

    let again =
        s.approvals
            .ask_if_changed(&s.ws, &entries(&["b.example", "a.example"]), never_asked);

    assert!(again.unwrap());
}

#[test]
fn changed_workspace_entries_are_asked_about_again() {
    let s = State::new();
    s.approvals
        .ask_if_changed(&s.ws, &entries(&["a.example"]), yes)
        .unwrap();

    let changed = s
        .approvals
        .ask_if_changed(&s.ws, &entries(&["a.example", "b.example"]), no);

    assert!(!changed.unwrap());
}

#[test]
fn declined_workspace_entries_are_not_applied_and_asked_about_next_time() {
    let s = State::new();
    let allow = entries(&["example.org"]);

    let declined = s.approvals.ask_if_changed(&s.ws, &allow, no);
    let next = s.approvals.ask_if_changed(&s.ws, &allow, yes);

    assert!(!declined.unwrap());
    assert!(next.unwrap());
}

#[test]
fn a_workspace_without_entries_is_never_asked() {
    let s = State::new();

    assert!(s.approvals.ask_if_changed(&s.ws, &[], never_asked).is_ok());
}

#[test]
fn an_approval_covers_only_its_own_workspace() {
    let s = State::new();
    let allow = entries(&["example.org"]);
    s.approvals.ask_if_changed(&s.ws, &allow, yes).unwrap();

    let other = s.approvals.ask_if_changed(&s.other, &allow, no);

    assert!(!other.unwrap());
}

#[test]
fn a_revoked_approval_is_asked_for_again() {
    let s = State::new();
    let allow = entries(&["example.org"]);
    s.approvals.ask_if_changed(&s.ws, &allow, yes).unwrap();
    s.approvals.ask_if_changed(&s.other, &allow, yes).unwrap();

    s.approvals.revoke(&s.ws).unwrap();

    assert!(!s.approvals.ask_if_changed(&s.ws, &allow, no).unwrap());
    assert!(
        s.approvals
            .ask_if_changed(&s.other, &allow, never_asked)
            .unwrap()
    );
}

#[test]
fn revoking_with_no_approvals_file_is_fine() {
    let s = State::new();

    assert!(s.approvals.revoke(&s.ws).is_ok());
}

#[test]
fn approvals_live_in_the_xdg_state_dir() {
    let s = State::new();
    let state = s.tmp.path().join("xdg-state");
    let home = s.tmp.path().join("home");
    let env = |xdg: bool| {
        let (state, home) = (state.clone(), home.clone());
        move |key: &str| match key {
            "XDG_STATE_HOME" if xdg => Some(state.to_string_lossy().into_owned()),
            "HOME" => Some(home.to_string_lossy().into_owned()),
            _ => None,
        }
    };
    let allow = entries(&["example.org"]);

    Approvals::from_env(env(true))
        .unwrap()
        .ask_if_changed(&s.ws, &allow, yes)
        .unwrap();
    Approvals::from_env(env(false))
        .unwrap()
        .ask_if_changed(&s.ws, &allow, yes)
        .unwrap();

    assert!(state.join("boxq/approvals.toml").is_file());
    assert!(home.join(".local/state/boxq/approvals.toml").is_file());
}
