use boxq::config::SetupHooks;
use boxq::setup::{Failed, Hook, Setup, Source};

fn done(command: Option<&str>) -> Hook {
    Hook {
        command: command.map(Into::into),
        done: true,
    }
}

fn hooks(global: Option<&str>, workspace: Option<&str>) -> SetupHooks {
    SetupHooks {
        global: global.map(Into::into),
        workspace: workspace.map(Into::into),
    }
}

#[test]
fn hooks_that_succeeded_as_configured_are_no_change() {
    let setup = Setup {
        global: done(Some("apt-get install -y jq")),
        workspace: done(None),
    };

    assert!(
        setup
            .changes(&hooks(Some("apt-get install -y jq"), None))
            .is_empty()
    );
}

#[test]
fn a_hook_that_succeeded_and_then_changed_is_named() {
    let setup = Setup {
        global: done(Some("a")),
        workspace: done(Some("npm ci")),
    };

    let changes = setup.changes(&hooks(Some("b"), Some("pnpm install")));

    assert_eq!(changes, ["Global Setup Hook", "Workspace Setup Hook"]);
}

/// The Sandbox was set up without it, which `boxq rm` then `boxq run` would change.
#[test]
fn a_hook_added_after_setup_is_a_change() {
    let setup = Setup {
        global: done(None),
        workspace: done(None),
    };

    assert_eq!(
        setup.changes(&hooks(None, Some("npm ci"))),
        ["Workspace Setup Hook"]
    );
}

/// It runs again from the config anyway.
#[test]
fn a_hook_that_has_not_succeeded_is_never_a_change() {
    let setup = Setup {
        global: Hook::default(),
        workspace: Hook {
            command: Some("exit 1".into()),
            done: false,
        },
    };

    assert!(setup.changes(&hooks(Some("a"), Some("b"))).is_empty());
}

#[test]
fn a_failed_hook_says_which_and_that_it_runs_again() {
    let failed = Failed {
        source: Source::Workspace,
        status: 3,
        interrupted: false,
    };

    assert_eq!(
        failed.to_string(),
        "The Workspace Setup Hook failed with status 3; the next `boxq run` or `boxq shell` \
         runs it again."
    );
}

#[test]
fn an_interrupted_hook_says_so() {
    let failed = Failed {
        source: Source::Global,
        status: 130,
        interrupted: true,
    };

    assert!(
        failed
            .to_string()
            .starts_with("The Global Setup Hook was interrupted; "),
        "{failed}"
    );
}
