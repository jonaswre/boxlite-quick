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
