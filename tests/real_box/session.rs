//! Interactive sessions on a terminal, through `boxq shell`: `src/session.rs` and the shell command
//! in `src/main.rs`.

use super::*;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn shell_opens_a_root_shell_in_the_workspace_and_leaves_the_sandbox_running() {
    let f = Fixture::new();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let ws = f.workspace.path().to_str().unwrap();

    let mut t = Terminal::boxq(&f, &["shell", ws]);
    t.expect_prompt(&f.workspace);
    // Typed `$` is echoed as-is, so only the shell's own output matches.
    t.type_in("echo \"at:$PWD:uid=$(id -u):$IS_SANDBOX:$TERM\"\r");
    t.expect(&format!("at:{ws}:uid=0:1:xterm-256color"));
    t.type_in("exit 3\r");

    assert_eq!(t.exit_code(), 3);
    let listed = f.sandboxes.list().await.unwrap();
    assert!(listed[0].running, "the Sandbox stopped with the session");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_session_follows_its_terminals_size() {
    let f = Fixture::new();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let mut t = Terminal::boxq(&f, &["shell", f.workspace.path().to_str().unwrap()]);
    t.expect_prompt(&f.workspace);
    t.type_in("echo \"size:$(stty size)\"\r");
    t.expect("size:24 80");

    t.resize(40, 100);
    // The resize and the typed command reach the guest on separate channels, so wait first.
    std::thread::sleep(std::time::Duration::from_secs(1));
    t.type_in("echo \"size:$(stty size)\"\r");

    t.expect("size:40 100");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn ctrl_c_interrupts_the_guests_command_not_boxq() {
    let f = Fixture::new();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let mut t = Terminal::boxq(&f, &["shell", f.workspace.path().to_str().unwrap()]);
    t.expect_prompt(&f.workspace);
    t.type_in("sleep 600\r");
    std::thread::sleep(std::time::Duration::from_secs(1));

    t.type_in("\x03");

    // The prompt is back long before the sleep ends, and a shell reports SIGINT as 130.
    t.expect_prompt(&f.workspace);
    t.type_in("echo \"interrupted:$?\"\r");
    t.expect("interrupted:130");
    t.type_in("exit\r");
    assert_eq!(t.exit_code(), 0);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_hangup_sent_to_boxq_reaches_the_session() {
    let f = Fixture::new();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let mut t = Terminal::boxq(&f, &["shell", f.workspace.path().to_str().unwrap()]);
    t.expect_prompt(&f.workspace);
    rustix::process::kill_process(
        rustix::process::Pid::from_child(&t.child),
        rustix::process::Signal::HUP,
    )
    .unwrap();

    // The guest's shell dies of SIGHUP, which a shell reports as 129.
    assert_eq!(t.exit_code(), 129);
}
