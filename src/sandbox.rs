//! The Sandbox lifecycle: one persistent Box per Workspace, found by its derived name
//! (jonaswre/boxlite-quick#10).

use crate::Workspace;
use crate::reap::reap_box;
use crate::runtime;
use anyhow::Context;
use boxlite::runtime::options::{BoxOptions, RootfsSpec};
use boxlite::{BoxCommand, BoxStatus, GuestSession};
use boxlite_shared::BoxTransport;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Every Sandbox under one boxq home.
pub struct Sandboxes {
    home: PathBuf,
}

/// The Sandbox `up` made ready.
pub struct Up {
    /// Whether `up` created the Sandbox, rather than reattaching to it.
    pub created: bool,
    pub target: ExecTarget,
}

/// One Sandbox as `list` reports it.
#[derive(Debug, PartialEq, Eq)]
pub struct Listed {
    /// `None` for a Box whose record is lost, which the Workspace's next `up` restores.
    pub workspace: Option<PathBuf>,
    pub box_name: String,
    pub running: bool,
}

/// What boxq keeps about a Sandbox that its Box can't carry.
#[derive(Serialize, Deserialize)]
struct Record {
    workspace: PathBuf,
}

/// What a command needs to run in a Sandbox with no runtime open: the Box's own socket, and the
/// container inside the Box that runs the image.
pub struct ExecTarget {
    socket: PathBuf,
    container_id: String,
}

impl ExecTarget {
    pub fn session(&self) -> GuestSession {
        GuestSession::new(BoxTransport::Unix {
            socket_path: self.socket.clone(),
        })
    }

    /// `program` as a command for this Sandbox. `LiteBox::exec` would name the container
    /// itself; a session on the socket has to.
    pub fn command(&self, program: &str) -> BoxCommand {
        BoxCommand::new(program)
            .env(
                "BOXLITE_EXECUTOR",
                format!("container={}", self.container_id),
            )
            .working_dir("/")
    }
}

impl Sandboxes {
    pub fn new(home: &Path) -> Self {
        Self { home: home.into() }
    }

    /// The Sandboxes under boxq's home: `BOXQ_HOME`, else `$XDG_DATA_HOME/boxq`, else
    /// `~/.local/share/boxq`. The home holds its own `BOXLITE_HOME`, so boxq never waits on
    /// another BoxLite tool's lock.
    pub fn from_process_env() -> anyhow::Result<Self> {
        let absolute = |key| std::env::var_os(key).filter(|dir| Path::new(dir).is_absolute());
        let home = match (absolute("BOXQ_HOME"), absolute("XDG_DATA_HOME")) {
            (Some(home), _) => PathBuf::from(home),
            (None, Some(data)) => Path::new(&data).join("boxq"),
            (None, None) => {
                let home =
                    std::env::var_os("HOME").context("cannot find boxq's home: HOME is not set")?;
                Path::new(&home).join(".local/share/boxq")
            }
        };
        Ok(Self::new(&home))
    }

    /// The `BOXLITE_HOME` holding the Sandboxes' Boxes and their image cache.
    pub fn boxlite_home(&self) -> PathBuf {
        self.home.join("boxlite")
    }

    /// Creates the Workspace's Sandbox from `image`, or reattaches to the one it has, and makes
    /// sure it runs.
    pub async fn up(&self, ws: &Workspace, image: &str) -> anyhow::Result<Up> {
        // The record goes first, so a Box never exists without one.
        self.write_record(ws)?;
        let rt = runtime::open(&self.boxlite_home())?;
        let options = BoxOptions {
            rootfs: RootfsSpec::Image(image.into()),
            entrypoint: Some(vec![]),
            cmd: Some(vec!["sleep".into(), "infinity".into()]),
            auto_delete: Some(0),
            detach: true,
            ..Default::default()
        };
        let (b, created) = rt.get_or_create(options, Some(ws.box_name())).await?;
        if b.info().await?.status != BoxStatus::Running {
            // A VM leaked by an earlier stop must never share the disk with this one.
            reap_box(&self.boxlite_home(), b.id().as_ref())?;
            b.start().await?;
        }
        let box_dir = self.boxlite_home().join("boxes").join(b.id().as_ref());
        Ok(Up {
            created,
            target: ExecTarget {
                socket: box_dir.join("sockets/box.sock"),
                container_id: container_id(&box_dir)?,
            },
        })
    }

    /// Stops the Workspace's Sandbox, keeping its state.
    pub async fn stop(&self, ws: &Workspace) -> anyhow::Result<()> {
        let rt = runtime::open(&self.boxlite_home())?;
        let Some(b) = rt.get(&ws.box_name()).await? else {
            anyhow::bail!("{} has no Sandbox", ws.path().display());
        };
        if b.info().await?.status == BoxStatus::Running {
            b.stop().await?;
        }
        reap_box(&self.boxlite_home(), b.id().as_ref())
    }

    /// Removes the Workspace's Sandbox and its state.
    pub async fn rm(&self, ws: &Workspace) -> anyhow::Result<()> {
        let rt = runtime::open(&self.boxlite_home())?;
        let Some(info) = rt.get_info(&ws.box_name()).await? else {
            anyhow::bail!("{} has no Sandbox", ws.path().display());
        };
        rt.remove(&ws.box_name(), true).await?;
        reap_box(&self.boxlite_home(), info.id.as_ref())?;
        let record = self.record_path(&ws.box_name());
        match std::fs::remove_file(&record) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("cannot remove {}", record.display()))
            }
            _ => Ok(()),
        }
    }

    /// Every Sandbox, ordered by Workspace path, with any whose record is lost last.
    pub async fn list(&self) -> anyhow::Result<Vec<Listed>> {
        let rt = runtime::open(&self.boxlite_home())?;
        let mut listed = Vec::new();
        for info in rt.list_info().await? {
            let box_name = info.name.unwrap_or_else(|| info.id.to_string());
            listed.push(Listed {
                workspace: self.read_record(&box_name)?.map(|r| r.workspace),
                box_name,
                running: info.status == BoxStatus::Running,
            });
        }
        listed.sort_by(|a, b| {
            (a.workspace.is_none(), &a.workspace, &a.box_name).cmp(&(
                b.workspace.is_none(),
                &b.workspace,
                &b.box_name,
            ))
        });
        Ok(listed)
    }

    fn record_path(&self, box_name: &str) -> PathBuf {
        self.home.join("sandboxes").join(format!("{box_name}.toml"))
    }

    fn write_record(&self, ws: &Workspace) -> anyhow::Result<()> {
        let path = self.record_path(&ws.box_name());
        let record = Record {
            workspace: ws.path().into(),
        };
        let toml = toml::to_string(&record)
            .with_context(|| format!("cannot record Workspace {}", ws.path().display()))?;
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        // Renamed into place, so a record is never read half-written.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("cannot write {}", path.display()))
    }

    fn read_record(&self, box_name: &str) -> anyhow::Result<Option<Record>> {
        let path = self.record_path(box_name);
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(
                toml::from_str(&s).with_context(|| format!("invalid {}", path.display()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }
}

/// The container id is `pub(crate)` in BoxLite 0.11; it is the one directory name under
/// `boxes/<id>/shared/containers/`.
fn container_id(box_dir: &Path) -> anyhow::Result<String> {
    let dir = box_dir.join("shared/containers");
    let mut ids = std::fs::read_dir(&dir)
        .with_context(|| format!("cannot list {}", dir.display()))?
        .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
        .collect::<anyhow::Result<Vec<_>>>()?;
    anyhow::ensure!(ids.len() == 1, "expected one container, found {ids:?}");
    Ok(ids.pop().unwrap())
}
