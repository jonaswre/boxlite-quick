use boxq::config::{Resources, Size};
use boxq::spec::{Spec, mismatch_warning};

#[test]
fn with_nothing_configured_a_sandbox_gets_the_interim_image_and_no_resources_set() {
    let spec = Spec::new(None, &Resources::default()).unwrap();

    assert_eq!(
        spec,
        Spec {
            image: "node:22-bookworm".into(),
            cpus: None,
            memory_mib: None,
            disk_gib: None,
        }
    );
}

fn size(s: &str) -> Option<Size> {
    Some(Size::try_from(s.to_string()).unwrap())
}

#[test]
fn the_configured_image_and_resources_are_used() {
    let resources = Resources {
        cpus: 2.try_into().ok(),
        memory: size("4GiB"),
        disk: size("20GiB"),
    };

    let spec = Spec::new(Some("ghcr.io/jonaswre/boxq:1"), &resources).unwrap();

    assert_eq!(
        spec,
        Spec {
            image: "ghcr.io/jonaswre/boxq:1".into(),
            cpus: Some(2),
            memory_mib: Some(4096),
            disk_gib: Some(20),
        }
    );
}

/// BoxLite sizes a disk in whole GiB; rounding up never gives a Sandbox less than it asked for.
#[test]
fn a_disk_size_short_of_a_whole_gib_is_rounded_up() {
    let resources = Resources {
        disk: size("1537MiB"),
        ..Default::default()
    };

    let spec = Spec::new(None, &resources).unwrap();

    assert_eq!(spec.disk_gib, Some(2));
}

#[test]
fn memory_beyond_what_boxlite_takes_is_an_error_naming_it() {
    let resources = Resources {
        memory: size("4194304GiB"),
        ..Default::default()
    };

    let err = Spec::new(None, &resources).unwrap_err();

    assert!(err.to_string().contains("4294967296MiB"), "{err}");
}

fn interim() -> Spec {
    Spec::new(None, &Resources::default()).unwrap()
}

#[test]
fn a_sandbox_created_as_configured_gets_no_warning() {
    let ws = std::path::Path::new("/home/me/project");

    assert_eq!(mismatch_warning(ws, &interim().changes(&interim())), None);
}

#[test]
fn a_changed_config_warns_in_one_line_naming_the_changes_and_the_fix() {
    let ws = std::path::Path::new("/home/me/project");
    let created = interim();
    let config = Spec {
        image: "alpine:3".into(),
        memory_mib: Some(2048),
        disk_gib: Some(20),
        ..interim()
    };

    let warning = mismatch_warning(ws, &config.changes(&created)).unwrap();

    assert_eq!(
        warning,
        "The Sandbox for /home/me/project was created before its image, memory and disk changed \
         in the config; to apply them, run `boxq rm /home/me/project` and then \
         `boxq run /home/me/project`, which discards the Sandbox's state."
    );
}

#[test]
fn a_single_change_is_named_alone() {
    let ws = std::path::Path::new("/w");
    let config = Spec {
        cpus: Some(4),
        ..interim()
    };

    let warning = mismatch_warning(ws, &config.changes(&interim())).unwrap();

    assert!(
        warning.starts_with("The Sandbox for /w was created before its cpus changed in"),
        "{warning}"
    );
}

/// The Setup Hooks that changed join the Spec's in the one line.
#[test]
fn changes_beyond_the_spec_are_named_in_the_same_line() {
    let ws = std::path::Path::new("/w");
    let config = Spec {
        image: "alpine:3".into(),
        ..interim()
    };
    let mut changed = config.changes(&interim());
    changed.push("Workspace Setup Hook");

    let warning = mismatch_warning(ws, &changed).unwrap();

    assert!(
        warning.starts_with(
            "The Sandbox for /w was created before its image and Workspace Setup Hook changed in"
        ),
        "{warning}"
    );
}
