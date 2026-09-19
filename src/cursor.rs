use crate::paths;
use crate::room::Message;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn path(root: &Path, agent: &str, room: &str) -> PathBuf {
    paths::cursors_dir(root).join(agent).join(room)
}

/// The header line of the last message this agent has read, if any.
pub fn load(root: &Path, agent: &str, room: &str) -> Option<String> {
    fs::read_to_string(path(root, agent, room))
        .ok()
        .map(|s| s.trim_end().to_string())
}

pub fn save(root: &Path, agent: &str, room: &str, header: &str) -> Result<()> {
    let p = path(root, agent, room);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&p, format!("{header}\n")).with_context(|| format!("writing cursor {}", p.display()))
}

/// Index of the first unread message. `None` when the agent has no cursor at all.
pub fn unread_index(messages: &[Message], cursor: Option<&str>) -> Option<usize> {
    let cur = cursor?;
    if cur.is_empty() {
        return Some(0);
    }
    if let Some(i) = messages.iter().rposition(|m| m.header() == cur) {
        return Some(i + 1);
    }
    // Header not found (archived, or edited): fall back to timestamp ordering.
    let ts = cur.rsplit_once("] - ").map(|(_, t)| t.trim()).unwrap_or("");
    if ts.is_empty() {
        return Some(0);
    }
    Some(
        messages
            .iter()
            .position(|m| m.timestamp.as_str() > ts)
            .unwrap_or(messages.len()),
    )
}
