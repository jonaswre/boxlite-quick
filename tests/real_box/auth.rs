//! The Agent's credentials, held outside the guest behind a placeholder: `src/auth.rs`.

use super::*;
use boxlite::runtime::options::{BoxOptions, RootfsSpec};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn the_guest_holds_only_the_placeholder_never_the_key() {
    let f = Fixture::new();

    let up = f.up(IMAGE).await;
    let token = sh(&up.target, "printf %s \"$ANTHROPIC_AUTH_TOKEN\"")
        .await
        .unwrap();
    // Every process's env and command line, and every file on the guest's own disk, read out
    // to be searched here: searching in the guest would need the key in the guest.
    let dump = sh(
        &up.target,
        "for p in /proc/[0-9]*; do cat $p/environ $p/cmdline; echo; done; cat /proc/cmdline; \
         find / -xdev -type f -exec cat {} + ; true",
    )
    .await
    .unwrap();

    assert_eq!(token, ("<BOXLITE_SECRET:anthropic>".into(), 0));
    assert!(
        dump.0.contains("<BOXLITE_SECRET:anthropic>"),
        "dump missed the env"
    );
    assert!(!dump.0.contains(FAKE_KEY), "the key is in the guest");
}

/// BoxLite fixes a Box's secrets and env at creation, so a rotated key or a moved base URL
/// reaches only a recreated Sandbox.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_sandbox_reports_when_it_was_created_with_other_auth() {
    let f = Fixture::new();
    let rotated = Auth::new(
        &BaseUrl::parse("https://cliproxy.boxq.invalid").unwrap(),
        "boxq-rotated-key",
    );
    let moved = Auth::new(
        &BaseUrl::parse("https://moved.boxq.invalid").unwrap(),
        FAKE_KEY,
    );
    let (spec, policy, same) = (spec(IMAGE), default_policy(), fake_auth());
    let up = |auth| f.sandboxes.up(&f.workspace, &spec, &policy, auth);

    let first = up(&f.auth).await.unwrap();
    let unchanged = up(&same).await.unwrap();
    let after_rotation = up(&rotated).await.unwrap();
    let after_move = up(&moved).await.unwrap();
    f.sandboxes.rm(&f.workspace).await.unwrap();
    let recreated = up(&rotated).await.unwrap();

    let changed = |up: &boxq::sandbox::Up| up.auth_changed;
    assert_eq!(
        [&first, &unchanged, &after_rotation, &after_move, &recreated].map(changed),
        [false, false, true, true, false]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_sandbox_without_a_record_of_its_auth_reports_no_change() {
    let f = Fixture::new();
    // The Workspace's Box with no record, as one created before boxq recorded its auth.
    let options = BoxOptions {
        rootfs: RootfsSpec::Image(IMAGE.into()),
        entrypoint: Some(vec![]),
        cmd: Some(vec!["sleep".into(), "infinity".into()]),
        auto_delete: Some(0),
        detach: true,
        ..Default::default()
    };
    let rt = f.runtime();
    rt.create(options, Some(f.workspace.box_name()))
        .await
        .unwrap();
    drop(rt);

    let up = f.up(IMAGE).await;

    assert!(!up.created);
    assert!(!up.auth_changed);
}

/// Needs a real base URL and key, so it reads them from `BOXQ_TEST_BASE_URL` and
/// `BOXQ_TEST_API_KEY`. Without them, as in CI, it skips, and says so: libtest has no skipped
/// result, so it still reports `ok`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box from the Base Image; needs KVM, internet access and a real base URL and key"]
async fn claude_code_authenticates_through_the_placeholder() {
    let (Ok(base_url), Ok(key)) = (
        std::env::var("BOXQ_TEST_BASE_URL"),
        std::env::var("BOXQ_TEST_API_KEY"),
    ) else {
        // libtest captures only the `print!` family, so this reaches the run's output.
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "auth::claude_code_authenticates_through_the_placeholder SKIPPED: \
             set BOXQ_TEST_BASE_URL and BOXQ_TEST_API_KEY to run it"
        );
        return;
    };
    let f = Fixture::new();
    let base_url = BaseUrl::parse(&base_url).unwrap();
    let auth = Auth::new(&base_url, &key);
    let egress = Egress {
        builtins: true,
        allow: vec![],
        allow_host_loopback: false,
        workspace_allow: vec![],
    };
    // The base URL's real addresses, which a tailnet host needs next to its name.
    let resolve = |host: &str| {
        use std::net::{SocketAddr, ToSocketAddrs};
        let addrs = (host, 443).to_socket_addrs().unwrap();
        let v4 = addrs.filter_map(|a| match a {
            SocketAddr::V4(a) => Some(*a.ip()),
            SocketAddr::V6(_) => None,
        });
        v4.collect()
    };
    let policy = Policy::new(&egress, Some(&base_url), false, resolve);

    let up = f
        .sandboxes
        .up(&f.workspace, &spec(&claude_image()), &policy, &auth)
        .await
        .unwrap();
    let asked = sh(
        &up.target,
        "claude -p 'Reply with the single word pong, in lower case.' 2>&1",
    )
    .await
    .unwrap();

    assert_eq!(asked.1, 0, "claude failed: {}", asked.0);
    assert!(asked.0.to_lowercase().contains("pong"), "{}", asked.0);
}
