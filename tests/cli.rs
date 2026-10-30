//! The `boxq` binary itself, for what no Box has to boot for. Tests that open the BoxLite runtime
//! still need KVM, so they're ignored here and run by CI's real-Box job.

use boxq::Workspace;
use boxq::egress::Approvals;
use std::path::Path;
use std::process::{Command, Output};

/// A temporary host with a Workspace whose Workspace Config entries have an Egress Approval.
struct Host {
    tmp: tempfile::TempDir,
    ws: Workspace,
}

const ALLOW: &[&str] = &["example.org"];

impl Host {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("project");
        std::fs::create_dir(&dir).unwrap();
        let host = Self {
            ws: Workspace::resolve(&dir).unwrap(),
            tmp,
        };
        host.approvals()
            .ask_if_changed(&host.ws, &allow(), |_| Ok(true))
            .unwrap();
        host
    }

    fn approvals(&self) -> Approvals {
        Approvals::new(&self.tmp.path().join("state/boxq/approvals.toml"))
    }

    /// Whether the Workspace's entries still have their Egress Approval.
    fn approved(&self) -> bool {
        self.approvals()
            .ask_if_changed(&self.ws, &allow(), |_| Ok(false))
            .unwrap()
    }

    /// Runs `boxq args` with `home` as boxq's home.
    fn boxq(&self, home: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_boxq"))
            .args(args)
            .env_clear()
            .env("HOME", self.tmp.path())
            .env("XDG_STATE_HOME", self.tmp.path().join("state"))
            .env("BOXQ_HOME", home)
            .output()
            .unwrap()
    }
}

fn allow() -> Vec<String> {
    ALLOW.iter().map(|&e| e.into()).collect()
}

#[test]
fn rm_keeps_the_egress_approval_when_the_sandbox_cannot_be_removed() {
    let host = Host::new();
    let ws = host.ws.path().to_str().unwrap();
    // A file where boxq's home should be, so the Sandbox can't even be looked up.
    let home = host.tmp.path().join("home-is-a-file");
    std::fs::write(&home, "").unwrap();

    let rm = host.boxq(&home, &["rm", ws]);

    assert!(!rm.status.success(), "{rm:?}");
    assert!(host.approved());
}

#[test]
#[ignore = "opens the BoxLite runtime; needs KVM"]
fn rm_forgets_the_egress_approval_even_without_a_sandbox() {
    let host = Host::new();
    let ws = host.ws.path().to_str().unwrap();
    let home = host.tmp.path().join("home");

    let rm = host.boxq(&home, &["rm", ws]);

    assert_eq!(
        String::from_utf8_lossy(&rm.stderr),
        format!("Error: {ws} has no Sandbox\n")
    );
    assert!(!host.approved());
}
