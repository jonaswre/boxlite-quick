//! What a Sandbox is created from: its image and resources (jonaswre/boxlite-quick#18). These
//! are fixed once the Sandbox exists, so a later config change only applies to a new one.

use crate::config::Resources;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// boxq's Base Image, which a Sandbox is created from when the config sets none. Pinned by
/// digest, so a cached copy is used with no registry round trip; bumped by hand to a build the
/// image workflow published (jonaswre/boxlite-quick#29).
pub const BASE_IMAGE: &str = "ghcr.io/jonaswre/boxq:2.1.312-247cf26@sha256:55a29d246f63c1788b39fec42d4067c11decb5a8843200566abc07abf3165709";

/// The image and resources to create a Sandbox with. A `None` resource leaves the choice to
/// BoxLite's default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    pub image: String,
    pub cpus: Option<u8>,
    pub memory_mib: Option<u32>,
    pub disk_gib: Option<u64>,
}

impl Spec {
    /// The Spec for the config's `image` and `[resources]`.
    pub fn new(image: Option<&str>, resources: &Resources) -> anyhow::Result<Self> {
        let memory_mib = resources
            .memory
            .map(|m| {
                u32::try_from(m.mib()).map_err(|_| {
                    anyhow::anyhow!("memory {}MiB is more than a Sandbox can have", m.mib())
                })
            })
            .transpose()?;
        Ok(Self {
            image: image.unwrap_or(BASE_IMAGE).into(),
            cpus: resources.cpus.map(|n| n.get()),
            memory_mib,
            // BoxLite sizes a disk in whole GiB; rounding up never gives less than asked for.
            disk_gib: resources.disk.map(|d| d.mib().div_ceil(1024)),
        })
    }

    /// The names of this Spec's settings that the Sandbox, created from `created`, lacks, for
    /// [`mismatch_warning`].
    pub fn changes(&self, created: &Spec) -> Vec<&'static str> {
        [
            ("image", self.image != created.image),
            ("cpus", self.cpus != created.cpus),
            ("memory", self.memory_mib != created.memory_mib),
            ("disk", self.disk_gib != created.disk_gib),
        ]
        .into_iter()
        .filter_map(|(name, changed)| changed.then_some(name))
        .collect()
    }
}

/// The one line telling the user that the Sandbox for `workspace` was created before the
/// `changed` settings changed in the config, or `None` if none did. boxq never rebuilds a
/// Sandbox on its own, since that discards its state.
pub fn mismatch_warning(workspace: &Path, changed: &[&str]) -> Option<String> {
    let (last, rest) = changed.split_last()?;
    let names = match rest {
        [] => last.to_string(),
        rest => format!("{} and {last}", rest.join(", ")),
    };
    Some(format!(
        "The Sandbox for {} was created before its {names} changed in the config; to apply \
         them, {}.",
        workspace.display(),
        recreate_hint(workspace)
    ))
}

/// How to get the Sandbox for `workspace` created afresh, to take what it was created from.
pub fn recreate_hint(workspace: &Path) -> String {
    let ws = workspace.display();
    format!("run `boxq rm {ws}` and then `boxq run {ws}`, which discards the Sandbox's state")
}
