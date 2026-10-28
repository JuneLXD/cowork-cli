use crate::config::Config;
use anyhow::{anyhow, bail, Result};
use std::env;

pub const DEFAULT_AGENTS: &[&str] = &["claude", "codex"];
/// The tools cowork has specific support for; the menus offer these first.
pub const KNOWN_AGENTS: &[&str] = &["claude", "codex", "kimi", "deepseek", "argon", "muse"];
/// The fewest agents a room can hold: one executor and at least one advisor to vote.
/// There is no upper limit.
pub const MIN_ROOM_AGENTS: usize = 2;

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
    // Muse Code documents these for every shell command it runs.
    if env::var_os("MUSE_SESSION_ID").is_some() || env::var_os("MUSE_THREAD_ID").is_some() {
        return Some(("muse".into(), "MUSE_SESSION_ID marker"));
    }
    // Antigravity CLI sets this for agent commands, though Google does not document it.
    // DeepSeek Harness's DSH_SHELL is deliberately not a marker: it can run any model.
    if env::var_os("ANTIGRAVITY_AGENT").is_some() {
        return Some(("argon".into(), "ANTIGRAVITY_AGENT marker"));
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
        // DeepSeek Harness marks its shells but can run any model, so the marker
        // cannot say which agent this is; it only explains what to set.
        if env::var_os("DSH_SHELL").is_some() {
            anyhow!("cannot determine which agent is calling: this is a DeepSeek Harness shell, which can run any model; set COWORK_AGENT=<NAME> (deepseek, or dsh-<model>) or pass --agent <NAME>")
        } else {
            anyhow!("cannot determine which agent is calling: pass --agent <NAME> or set COWORK_AGENT=<NAME> (see `cowork doctor`)")
        }
    })
}

/// `dsh-<model>` names an agent that runs <model> in DeepSeek Harness, which can drive
/// models from any provider. It gets DeepSeek Harness's instructions, like `deepseek`.
pub const DSH_PREFIX: &str = "dsh-";

/// The known tool an agent works in: its own name, or `deepseek` for `dsh-<model>`.
pub fn known_tool(agent: &str) -> Option<&'static str> {
    let a = agent.trim().to_ascii_lowercase();
    let a = match a.strip_prefix(DSH_PREFIX) {
        Some(model) if !model.is_empty() => "deepseek",
        _ => a.as_str(),
    };
    KNOWN_AGENTS.iter().copied().find(|k| *k == a)
}

/// The tool an agent works in, for messages: "DeepSeek Harness" for `dsh-qwen` too.
pub fn tool_name(agent: &str) -> Option<&'static str> {
    Some(match known_tool(agent)? {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "kimi" => "Kimi Code",
        "deepseek" => "DeepSeek Harness",
        "argon" => "Google Antigravity CLI",
        "muse" => "Meta Muse Code",
        _ => return None,
    })
}

/// What the pickers show next to an agent name: its tool, and the model when the
/// name alone does not say which tool runs it.
pub fn describe(agent: &str) -> Option<String> {
    let a = agent.trim().to_ascii_lowercase();
    if let Some(model) = a.strip_prefix(DSH_PREFIX).filter(|m| !m.is_empty()) {
        return Some(format!("{model} in DeepSeek Harness"));
    }
    match known_tool(&a)? {
        "argon" => Some("Gemini 4 Argon in Google Antigravity CLI".to_string()),
        _ => tool_name(&a).map(str::to_string),
    }
}

pub fn participants(cfg: &Config) -> Vec<String> {
    cfg.agents
        .clone()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| DEFAULT_AGENTS.iter().map(|s| s.to_string()).collect())
}

/// A room roster must be at least MIN_ROOM_AGENTS distinct, valid names. Checked
/// before anything is written, so a bad list never leaves a room behind.
pub fn validate_roster(list: &[String]) -> Result<()> {
    if list.len() < MIN_ROOM_AGENTS {
        bail!(
            "a room needs at least {MIN_ROOM_AGENTS} agents (one executor and at least one advisor), got {} ({})",
            list.len(),
            list.join(", ")
        );
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
