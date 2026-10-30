use boxq::Workspace;

#[test]
fn a_workspace_is_identified_by_its_canonical_absolute_path() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    std::fs::create_dir_all(dir.join("src")).unwrap();

    let ws = Workspace::resolve(&dir.join("src/..")).unwrap();

    assert_eq!(ws.path(), std::fs::canonicalize(&dir).unwrap());
}

#[test]
fn a_symlink_resolves_to_the_workspace_it_points_at() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    std::fs::create_dir(&dir).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();

    assert_eq!(
        Workspace::resolve(&link).unwrap(),
        Workspace::resolve(&dir).unwrap()
    );
}

#[test]
fn a_missing_path_is_an_error_naming_it() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope");

    let err = Workspace::resolve(&missing).unwrap_err();

    assert!(
        err.to_string().contains(&*missing.to_string_lossy()),
        "{err}"
    );
}

// Expected hashes come from `printf '%s' <path> | sha256sum`, so a Box name never
// changes between boxq builds and existing Sandboxes are always found again.
#[test]
fn the_box_name_is_the_dirname_and_a_stable_hash_of_the_path() {
    let ws = Workspace::resolve(std::path::Path::new("/usr/share")).unwrap();

    assert_eq!(ws.box_name(), "boxq-share-10b207998825");
}

fn hash_part<'a>(name: &'a str, prefix: &str) -> &'a str {
    let hash = name
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("{name} lacks {prefix}"));
    assert!(
        hash.len() == 12
            && hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "{name}"
    );
    hash
}

#[test]
fn the_dirname_in_a_box_name_is_slugged() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("My Project_v2.0!");
    std::fs::create_dir(&dir).unwrap();

    hash_part(
        &Workspace::resolve(&dir).unwrap().box_name(),
        "boxq-my-project-v2-0-",
    );
}

#[test]
fn a_workspace_with_no_usable_dirname_is_named_by_hash_alone() {
    let ws = Workspace::resolve(std::path::Path::new("/")).unwrap();

    assert_eq!(ws.box_name(), "boxq-8a5edab28263");
}

#[test]
fn a_long_dirname_is_cut_to_24_characters() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp
        .path()
        .join("an-extremely-long-directory-name-for-a-project");
    std::fs::create_dir(&dir).unwrap();

    hash_part(
        &Workspace::resolve(&dir).unwrap().box_name(),
        "boxq-an-extremely-long-direct-",
    );
}

#[test]
fn a_file_is_not_a_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "").unwrap();

    let err = Workspace::resolve(&file).unwrap_err();

    assert!(err.to_string().contains("not a directory"), "{err}");
}

/// Runs host git in `dir`, failing the test if git does.
fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=boxq", "-c", "user.email=boxq@example.com"])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repo with one commit at `<tmp>/repo`.
fn repo(tmp: &std::path::Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    repo
}

#[test]
fn a_worktree_shares_its_repos_common_git_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    git(&repo, &["worktree", "add", "-q", "../feature"]);

    let ws = Workspace::resolve(&tmp.path().join("feature")).unwrap();

    let git_dir = std::fs::canonicalize(repo.join(".git")).unwrap();
    assert_eq!(ws.shared_dirs(), [ws.path().to_path_buf(), git_dir]);
}

#[test]
fn a_submodule_shares_its_git_dir_in_the_superproject() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = repo(tmp.path());
    let sup = tmp.path().join("super");
    std::fs::create_dir(&sup).unwrap();
    git(&sup, &["init", "-q"]);
    git(
        &sup,
        &["submodule", "add", "-q", lib.to_str().unwrap(), "lib"],
    );

    let ws = Workspace::resolve(&sup.join("lib")).unwrap();

    let git_dir = std::fs::canonicalize(sup.join(".git/modules/lib")).unwrap();
    assert_eq!(ws.shared_dirs(), [ws.path().to_path_buf(), git_dir]);
}

#[test]
fn a_git_dir_inside_the_workspace_is_not_shared_again() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "--separate-git-dir", "repo.git"]);

    let ws = Workspace::resolve(&dir).unwrap();

    assert_eq!(ws.shared_dirs(), [ws.path().to_path_buf()]);
}

#[test]
fn a_plain_repo_shares_only_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();

    let ws = Workspace::resolve(&repo(tmp.path())).unwrap();

    assert_eq!(ws.shared_dirs(), [ws.path().to_path_buf()]);
}

/// git on the host fails the same way, so the Sandbox is no worse off.
#[test]
fn a_git_file_pointing_nowhere_shares_only_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(".git"), "gitdir: /nonexistent/.git/worktrees/x\n").unwrap();

    let ws = Workspace::resolve(&dir).unwrap();

    assert_eq!(ws.shared_dirs(), [ws.path().to_path_buf()]);
}
