use crate::config::Config;
use anyhow::{anyhow, bail, Result};
use std::env;

pub const DEFAULT_AGENTS: &[&str] = &["claude", "codex"];
/// The tools cowork has specific support for; the menus offer these first.
pub const KNOWN_AGENTS: &[&str] = &["claude", "codex", "kimi"];
/// The most agents one room can hold: one executor and up to two advisors.
pub const MAX_ROOM_AGENTS: usize = 3;

/// Detect the calling agent from the environment. Returns (name, source).
pub fn detect_env() -> Option<(String, &'static str)> {
    for var in ["COWORK_AGENT", "ROOM_AGENT"] {
        if let Ok(a) = env::var(var) {
            if !a.trim().is_empty() {
                let src: &'static str = if var == "COWORK_AGENT" { "COWORK_AGENT" } else { "ROOM_AGENT (legacy name)" };
                return Some((a.trim().to_string(), src));
            }
        }
    }
    if env::var_os("CLAUDECODE").is_some() || env::var_os("CLAUDE_CODE_ENTRYPOINT").is_some() {
        return Some(("claude".into(), "CLAUDECODE marker"));
    }
    for (k, _) in env::vars_os() {
        let k = k.to_string_lossy();
        if k.starts_with("CODEX_") && k != "CODEX_HOME" {
            return Some(("codex".into(), "CODEX_* marker"));
        }
    }
    None
}

pub fn detect(flag: Option<&str>) -> Result<String> {
    if let Some(f) = flag {
        if !f.trim().is_empty() {
            return Ok(f.trim().to_string());
        }
    }
    detect_env().map(|(a, _)| a).ok_or_else(|| {
        anyhow!("cannot determine which agent is calling: pass --agent <NAME> or set COWORK_AGENT=<NAME> (see `cowork doctor`)")
    })
}

pub fn participants(cfg: &Config) -> Vec<String> {
    cfg.agents
        .clone()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| DEFAULT_AGENTS.iter().map(|s| s.to_string()).collect())
}

/// A room roster must be 2 to MAX_ROOM_AGENTS distinct, valid names. Checked
/// before anything is written, so a bad list never leaves a room behind.
pub fn validate_roster(list: &[String]) -> Result<()> {
    if list.len() < 2 || list.len() > MAX_ROOM_AGENTS {
        bail!("a room needs 2 to {MAX_ROOM_AGENTS} agents, got {} ({})", list.len(), list.join(", "));
    }
    for (i, a) in list.iter().enumerate() {
        crate::room::validate_agent(a)?;
        if list[..i].iter().any(|b| b.eq_ignore_ascii_case(a)) {
            bail!("agent `{a}` is listed twice");
        }
    }
    Ok(())
}

/// Everyone who is not `me`, joined for display.
pub fn counterpart(me: &str, participants: &[String]) -> String {
    let others: Vec<&str> = participants
        .iter()
        .map(String::as_str)
        .filter(|a| !a.eq_ignore_ascii_case(me))
        .collect();
    match others.len() {
        0 => "your counterpart".to_string(),
        1 => others[0].to_string(),
        _ => others.join(", "),
    }
}
