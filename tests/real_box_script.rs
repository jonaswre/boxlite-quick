//! `scripts/real-box`, the one command that runs the real-Box tests (jonaswre/boxlite-quick#42).
//! A stub `cargo` stands in for the build and the tests, so no Box boots here.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

/// A temporary host whose `cargo` is a stub.
struct Host {
    tmp: tempfile::TempDir,
}

impl Host {
    /// A host whose `cargo test --no-run` runs `build` and whose `cargo test` runs `tests`, both
    /// as shell scripts.
    fn new(build: &str, tests: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let cargo = bin.join("cargo");
        std::fs::write(
            &cargo,
            format!(
                "#!/bin/sh\ncase \" $* \" in\n*' --no-run '*)\n{build}\n;;\n*)\n{tests}\n;;\nesac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(tmp.path().join("tmp")).unwrap();
        Self { tmp }
    }

    /// Runs `scripts/real-box args` with a time limit of `limit` seconds.
    fn run(&self, limit: u32, args: &[&str]) -> Output {
        self.run_with(limit, args, &[])
    }

    /// [`Host::run`] with `env` added to the stub's environment.
    fn run_with(&self, limit: u32, args: &[&str], env: &[(&str, &OsStr)]) -> Output {
        let path = format!(
            "{}:{}",
            self.tmp.path().join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/real-box"))
            .args(args)
            .envs(env.iter().copied())
            .env("PATH", path)
            .env("TMPDIR", self.tmp.path().join("tmp"))
            .env("CARGO_TARGET_DIR", self.tmp.path().join("target"))
            .env("REAL_BOX_TIME_LIMIT", limit.to_string())
            .output()
            .unwrap()
    }

    /// How many files the run left in the temporary directory.
    fn temporary_files(&self) -> usize {
        std::fs::read_dir(self.tmp.path().join("tmp"))
            .unwrap()
            .count()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_passing_run_saves_its_output_to_a_log_and_leaves_no_temporary_files() {
    let host = Host::new(
        "echo built",
        "mktemp -d >/dev/null; echo \"test egress::x ... ok $*\"",
    );
    let out = host.run(60, &["egress::"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let logs: Vec<_> = std::fs::read_dir(host.tmp.path().join("target/real-box"))
        .unwrap()
        .collect();
    let log = std::fs::read_to_string(logs[0].as_ref().unwrap().path()).unwrap();
    assert!(log.contains("built"), "{log}");
    assert!(log.contains("--ignored egress::"), "{log}");
    assert_eq!(host.temporary_files(), 0);
    assert!(stderr(&out).contains("passed"), "{}", stderr(&out));
}

#[test]
fn a_failing_test_exits_non_zero_and_says_a_test_failed() {
    let host = Host::new("exit 0", "echo 'test egress::x ... FAILED'; exit 101");
    let out = host.run(60, &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("a test failed"), "{}", stderr(&out));
}

#[test]
fn a_failed_build_exits_non_zero_without_running_the_tests() {
    let host = Host::new("exit 101", "echo tests ran >&2");
    let out = host.run(60, &[]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("the build failed"),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains("tests ran"), "{}", stderr(&out));
}

#[test]
fn a_build_past_the_time_limit_is_stopped_and_says_the_build_hit_it() {
    let host = Host::new("sleep 30", "echo tests ran >&2");
    let out = host.run(1, &[]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("the build hit the time limit of 1 s"),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains("tests ran"), "{}", stderr(&out));
}

#[test]
fn a_run_past_the_time_limit_is_stopped_and_says_the_limit_was_hit() {
    let host = Host::new("exit 0", "sleep 30");
    let start = std::time::Instant::now();
    let out = host.run(2, &[]);
    assert!(start.elapsed().as_secs() < 10);
    assert_eq!(out.status.code(), Some(124));
    assert!(
        stderr(&out).contains("the tests hit the time limit of 2 s"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_vms_jailer_writing_after_the_vm_dies_leaves_no_temporary_files() {
    // BoxLite's bwrap jailer names the Box's home on its command line and outlives its VM.
    let host = Host::new(
        "exit 0",
        "home=$(mktemp -d); cp \"$(command -v sleep)\" \"$home/boxlite-shim\"
         setsid sh -c '\"$0/boxlite-shim\" 600; sleep 0.5; mkdir -p \"$0/exit\"' \"$home\" \
           >/dev/null 2>&1 &
         sleep 30",
    );
    let out = host.run(2, &[]);
    assert!(!out.status.success());
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert_eq!(host.temporary_files(), 0);
}

#[test]
fn no_vm_a_stopped_test_left_outlives_the_run() {
    // A Box's VM runs a copy of boxlite-shim in the test's home, in its own session.
    let host = Host::new(
        "exit 0",
        "home=$(mktemp -d); cp \"$(command -v sleep)\" \"$home/boxlite-shim\"
         setsid \"$home/boxlite-shim\" 600 & echo $! > \"$PIDFILE\"; sleep 30",
    );
    let pidfile = host.tmp.path().join("pid");
    let start = std::time::Instant::now();
    let out = host.run_with(2, &[], &[("PIDFILE", pidfile.as_os_str())]);
    assert!(start.elapsed().as_secs() < 10);
    assert!(!out.status.success());
    let pid = std::fs::read_to_string(&pidfile).unwrap();
    assert!(
        std::fs::read_link(format!("/proc/{}/exe", pid.trim())).is_err(),
        "the shim {} still runs",
        pid.trim()
    );
}
