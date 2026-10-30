//! A fake shim is a copy of `/bin/sh` placed where BoxLite puts a Box's `boxlite-shim`, so these
//! tests need no KVM.

use boxq::reap::reap_box;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Starts a fake shim for `box_id` under `home` that blocks reading stdin, running `prelude` first.
fn fake_shim(home: &Path, box_id: &str, prelude: &str) -> Child {
    let bin = home.join("boxes").join(box_id).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("boxlite-shim");
    std::fs::copy("/bin/sh", &shim).unwrap();
    let mut child = loop {
        match Command::new(&shim)
            .args(["-c", &format!("{prelude} echo ready; read x")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            // A parallel test forking while our copy was open for writing still holds it.
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            spawned => break spawned.unwrap(),
        }
    };
    // Wait until the shell runs, so its signal dispositions are in place.
    let mut ready = [0; 6];
    std::io::Read::read_exact(child.stdout.as_mut().unwrap(), &mut ready).unwrap();
    child
}

/// The signal that ended `child`, waiting briefly for it to be reaped.
fn killed_by(child: &mut Child) -> Option<i32> {
    for _ in 0..50 {
        if let Some(status) = child.try_wait().unwrap() {
            return status.signal();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}

#[test]
fn a_boxs_shim_is_terminated_gracefully() {
    let home = tempfile::tempdir().unwrap();
    let mut shim = fake_shim(home.path(), "box1", "");

    reap_box(home.path(), "box1").unwrap();

    assert_eq!(killed_by(&mut shim), Some(libc::SIGTERM));
}

#[test]
fn a_shim_that_ignores_sigterm_is_killed_after_the_grace_period() {
    let home = tempfile::tempdir().unwrap();
    let mut shim = fake_shim(home.path(), "box1", "trap '' TERM;");

    reap_box(home.path(), "box1").unwrap();

    assert_eq!(killed_by(&mut shim), Some(libc::SIGKILL));
}

#[test]
fn a_shim_is_reaped_after_its_box_directory_is_deleted() {
    let home = tempfile::tempdir().unwrap();
    let mut shim = fake_shim(home.path(), "box1", "");
    std::fs::remove_dir_all(home.path().join("boxes/box1")).unwrap();

    reap_box(home.path(), "box1").unwrap();

    assert_eq!(killed_by(&mut shim), Some(libc::SIGTERM));
}

#[test]
fn another_boxs_shim_is_left_running() {
    let home = tempfile::tempdir().unwrap();
    let mut other = fake_shim(home.path(), "box10", "");

    reap_box(home.path(), "box1").unwrap();

    let still_running = other.try_wait().unwrap().is_none();
    other.kill().unwrap();
    assert!(still_running);
}

#[test]
fn a_home_reached_through_a_symlink_still_finds_the_shim() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let mut shim = fake_shim(&home, "box1", "");

    reap_box(&link, "box1").unwrap();

    assert_eq!(killed_by(&mut shim), Some(libc::SIGTERM));
}
