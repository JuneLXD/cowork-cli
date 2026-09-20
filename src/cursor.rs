use crate::paths;
use crate::room::Message;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn path(root: &Path, agent: &str, room: &str) -> PathBuf {
    paths::cursors_dir(root).join(agent).join(room)
}

/// The cursor token of the last message this agent has read, if any: `id:N`,
/// or, for cursors written before ids existed, the header line of that message.
pub fn load(root: &Path, agent: &str, room: &str) -> Option<String> {
    fs::read_to_string(path(root, agent, room))
        .ok()
        .map(|s| s.trim_end().to_string())
}

pub fn save(root: &Path, agent: &str, room: &str, token: &str) -> Result<()> {
    let p = path(root, agent, room);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&p, format!("{token}\n")).with_context(|| format!("writing cursor {}", p.display()))
}

/// Index of the first unread message. `None` when the agent has no cursor at all.
/// When the cursor cannot be matched exactly, the fallback errs on the side of
/// showing a message again rather than skipping it.
pub fn unread_index(messages: &[Message], cursor: Option<&str>) -> Option<usize> {
    let cur = cursor?;
    if cur.is_empty() {
        return Some(0);
    }
    if let Some(id) = cur.strip_prefix("id:").and_then(|n| n.trim().parse::<u64>().ok()) {
        if let Some(i) = messages.iter().position(|m| m.id == Some(id)) {
            return Some(i + 1);
        }
        // The message was archived: everything with a greater id is unread.
        return Some(
            messages
                .iter()
                .position(|m| m.id.map(|x| x > id).unwrap_or(false))
                .unwrap_or(messages.len()),
        );
    }
    // Legacy header cursor. Only a message without an id can be the one it named:
    // a newer id-bearing post that happens to share the header is replayed, not skipped.
    if let Some(i) = messages.iter().position(|m| m.id.is_none() && m.header() == cur) {
        return Some(i + 1);
    }
    // Header not found (archived, or edited): fall back to timestamp ordering,
    // replaying anything posted in the same second.
    let ts = cur.rsplit_once("] - ").map(|(_, t)| t.trim()).unwrap_or("");
    if ts.is_empty() {
        return Some(0);
    }
    Some(
        messages
            .iter()
            .position(|m| m.timestamp.as_str() >= ts)
            .unwrap_or(messages.len()),
    )
}
