use crate::{config, registry};
use anyhow::{anyhow, bail, Result};
use std::env;
use std::path::{Path, PathBuf};

pub const AI_DIR: &str = ".ai-common";

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct RoomRef {
    pub project: Project,
    pub room: String,
    pub path: PathBuf,
}

impl RoomRef {
    pub fn addr(&self) -> String {
        format!("{}/{}", self.project.name, self.room)
    }
}

pub fn dir_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".into())
}

pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

pub fn find_ai_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join(AI_DIR).is_dir())
        .map(Path::to_path_buf)
}

pub fn project_at(root: PathBuf) -> Project {
    let name = config::load(&root)
        .name
        .unwrap_or_else(|| dir_name(&root));
    Project { root, name }
}

pub fn current_project() -> Result<Project> {
    let cwd = env::current_dir()?;
    let root = find_ai_root(&cwd)
        .or_else(|| find_git_root(&cwd))
        .ok_or_else(|| {
            anyhow!(
                "not inside a project (no {AI_DIR}/ or .git found above {}). Run `room init` inside a git repository.",
                cwd.display()
            )
        })?;
    Ok(project_at(root))
}

pub fn ai_dir(root: &Path) -> PathBuf {
    root.join(AI_DIR)
}
pub fn rooms_dir(root: &Path) -> PathBuf {
    ai_dir(root).join("rooms")
}
pub fn cursors_dir(root: &Path) -> PathBuf {
    ai_dir(root).join(".cursors")
}
pub fn templates_dir(root: &Path) -> PathBuf {
    ai_dir(root).join("templates")
}
pub fn archive_dir(root: &Path) -> PathBuf {
    ai_dir(root).join("archive")
}
pub fn config_path(root: &Path) -> PathBuf {
    ai_dir(root).join("room.toml")
}
pub fn protocol_path(root: &Path) -> PathBuf {
    ai_dir(root).join("PROTOCOL.md")
}
pub fn onboarding_path(root: &Path) -> PathBuf {
    ai_dir(root).join("ONBOARDING.md")
}
pub fn room_path(root: &Path, room: &str) -> PathBuf {
    rooms_dir(root).join(format!("{room}.md"))
}

pub fn is_initialized(root: &Path) -> bool {
    rooms_dir(root).is_dir()
}

pub fn ensure_initialized(p: &Project) -> Result<()> {
    if !is_initialized(&p.root) {
        bail!(
            "project `{}` is not initialized: run `room init` in {}",
            p.name,
            p.root.display()
        );
    }
    Ok(())
}

pub fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn data_dir() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/share"))
        .join("room")
}

pub fn registry_path() -> PathBuf {
    data_dir().join("projects.toml")
}

/// Unix socket paths are limited to ~108 bytes, so fall back to /tmp when the
/// preferred location is too long.
pub fn socket_path() -> PathBuf {
    let preferred = env::var_os("XDG_RUNTIME_DIR")
        .map(|d| PathBuf::from(d).join("room.sock"))
        .unwrap_or_else(|| data_dir().join("room.sock"));
    if preferred.as_os_str().len() < 100 {
        return preferred;
    }
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata(home_dir()).map(|m| m.uid()).unwrap_or(0);
    PathBuf::from(format!("/tmp/room-{uid}.sock"))
}

pub fn log_path() -> PathBuf {
    data_dir().join("daemon.log")
}

pub fn pid_path() -> PathBuf {
    data_dir().join("daemon.pid")
}

pub fn validate_room_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !ok {
        bail!("invalid room name `{name}`: use letters, digits, `-`, `_`, `.`");
    }
    Ok(())
}

fn registry_project(name: &str) -> Result<Project> {
    let root = registry::lookup(name).ok_or_else(|| {
        anyhow!("unknown project `{name}`: run `room init` there first, or check `room list --all`")
    })?;
    Ok(Project {
        root,
        name: name.to_string(),
    })
}

/// Resolve `<room>` or `<project>/<room>`. Falls back to `ROOM_ID`, then `main`.
pub fn resolve_room(addr: Option<&str>) -> Result<RoomRef> {
    let addr = addr
        .map(String::from)
        .or_else(|| env::var("ROOM_ID").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "main".to_string());
    let (proj_name, room) = match addr.split_once('/') {
        Some((p, r)) => (Some(p.to_string()), r.to_string()),
        None => (None, addr.clone()),
    };
    validate_room_name(&room)?;
    let project = match proj_name {
        None => current_project()?,
        Some(p) => match current_project() {
            Ok(cur) if cur.name == p => cur,
            _ => registry_project(&p)?,
        },
    };
    let path = room_path(&project.root, &room);
    Ok(RoomRef {
        project,
        room,
        path,
    })
}

pub fn ensure_room_exists(rr: &RoomRef) -> Result<()> {
    ensure_initialized(&rr.project)?;
    if !rr.path.exists() {
        bail!(
            "room `{}` does not exist in project `{}`. Create it with `room new {}`.",
            rr.room,
            rr.project.name,
            rr.room
        );
    }
    Ok(())
}
