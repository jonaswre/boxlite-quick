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
        self.command(home, args).output().unwrap()
    }

    fn command(&self, home: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_boxq"));
        cmd.args(args)
            .env_clear()
            .env("HOME", self.tmp.path())
            .env("XDG_STATE_HOME", self.tmp.path().join("state"))
            .env("BOXQ_HOME", home);
        cmd
    }
}

/// A `bwrap` that BoxLite takes for the system one, but that can't create a user namespace, as
/// on a host whose AppArmor has no userns profile for it.
const BWRAP_WITHOUT_USERNS: &str = "#!/bin/sh
if [ \"$1\" = --version ]; then echo 'bubblewrap 0.9.0'; exit 0; fi
echo 'bwrap: setting up uid map: Permission denied' >&2
exit 1
";

#[test]
#[ignore = "opens the BoxLite runtime; needs KVM"]
fn a_sandbox_that_cannot_start_names_the_step_and_prints_boxlites_report() {
    let host = Host::new();
    let ws = host.ws.path().to_str().unwrap();
    std::fs::write(host.ws.path().join(".boxq.toml"), "image = \"alpine:3\"\n").unwrap();
    let bin = host.tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let bwrap = bin.join("bwrap");
    std::fs::write(&bwrap, BWRAP_WITHOUT_USERNS).unwrap();
    std::fs::set_permissions(&bwrap, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let run = host
        .command(&host.tmp.path().join("home"), &["run", ws])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("BOXQ_BASE_URL", "https://cliproxy.boxq.invalid")
        .env("BOXQ_API_KEY", "boxq-fake-key")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("Error: cannot start the Sandbox for {ws}")),
        "{stderr}"
    );
    // BoxLite's own error says only this; its report goes to tracing.
    assert!(
        stderr.contains("The sandbox could not be initialized on this host."),
        "{stderr}"
    );
    assert!(
        stderr.contains("bwrap stderr: bwrap: setting up uid map: Permission denied"),
        "{stderr}"
    );
}

#[test]
fn a_runtime_that_cannot_open_is_told_apart_from_a_sandbox_that_cannot_start() {
    let host = Host::new();
    let ws = host.ws.path().to_str().unwrap();
    // A file where boxq's home should be, so BoxLite's runtime can't open in it.
    let home = host.tmp.path().join("home-is-a-file");
    std::fs::write(&home, "").unwrap();

    let run = host
        .command(&home, &["run", ws])
        .env("BOXQ_BASE_URL", "https://cliproxy.boxq.invalid")
        .env("BOXQ_API_KEY", "boxq-fake-key")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "Error: cannot open the runtime for the Sandbox for {ws}"
        )),
        "{stderr}"
    );
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
