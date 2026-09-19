use crate::config::Config;
use anyhow::{anyhow, Result};
use std::env;

pub const DEFAULT_AGENTS: &[&str] = &["claude", "codex"];

/// Detect the calling agent from the environment. Returns (name, source).
pub fn detect_env() -> Option<(String, &'static str)> {
    if let Ok(a) = env::var("ROOM_AGENT") {
        if !a.trim().is_empty() {
            return Some((a.trim().to_string(), "ROOM_AGENT"));
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
        anyhow!("cannot determine which agent is calling: pass --agent <NAME> or set ROOM_AGENT=<NAME> (see `room doctor`)")
    })
}

pub fn participants(cfg: &Config) -> Vec<String> {
    cfg.agents
        .clone()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| DEFAULT_AGENTS.iter().map(|s| s.to_string()).collect())
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
