use anyhow::Context;
use sha2::{Digest, Sha256};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The host directory shared into a Sandbox, identified by its canonical absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    path: PathBuf,
}

impl Workspace {
    pub fn resolve(path: &Path) -> anyhow::Result<Self> {
        let path = std::fs::canonicalize(path)
            .with_context(|| format!("cannot resolve Workspace {}", path.display()))?;
        anyhow::ensure!(
            path.is_dir(),
            "Workspace {} is not a directory",
            path.display()
        );
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The host directories shared into the Sandbox, each at its own path: the Workspace, and
    /// the git directory a worktree or submodule keeps outside it, so git works unchanged in the
    /// Sandbox.
    pub fn shared_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.path.clone()];
        dirs.extend(self.outside_git_dir());
        dirs
    }

    /// The common git directory a `.git` file points to, if it is outside the Workspace. A
    /// worktree's `.git` file points into its repo's `.git`, which `commondir` names; a
    /// submodule's points at its own. A `.git` file git can't follow on the host is ignored, so
    /// git fails in the Sandbox just as it does there.
    fn outside_git_dir(&self) -> Option<PathBuf> {
        let file = std::fs::read_to_string(self.path.join(".git")).ok()?;
        let git_dir = self.path.join(file.strip_prefix("gitdir: ")?.trim_end());
        let common = match std::fs::read_to_string(git_dir.join("commondir")) {
            Ok(common) => git_dir.join(common.trim_end()),
            Err(_) => git_dir,
        };
        std::fs::canonicalize(common)
            .ok()
            .filter(|dir| !dir.starts_with(&self.path))
    }

    /// The name of the Box backing this Workspace's Sandbox. It must never change for a
    /// given path, or boxq would lose track of existing Sandboxes.
    pub fn box_name(&self) -> String {
        let digest = Sha256::digest(self.path.as_os_str().as_bytes());
        let hash: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
        match slug(&self.path.file_name().unwrap_or_default().to_string_lossy()) {
            dirname if dirname.is_empty() => format!("boxq-{hash}"),
            dirname => format!("boxq-{dirname}-{hash}"),
        }
    }
}

/// Lowercase ASCII alphanumerics, with every other run of characters collapsed to one `-`,
/// cut to 24 characters.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if out.len() == 24 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}
