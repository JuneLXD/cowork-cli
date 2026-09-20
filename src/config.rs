use crate::paths;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_timeout: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_keep: Option<usize>,
    /// Seconds a Stop hook waits for new messages before letting the agent stop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_wait: Option<u64>,
    /// Max consecutive continues a Stop hook may force in one session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_max_continues: Option<u64>,
    /// Override for where `room feedback` sends reports (a Supabase project URL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_url: Option<String>,
    /// The publishable (insert-only) key for that endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_key: Option<String>,
}

pub const KEYS: &[&str] = &["name", "agents", "tail", "wait_timeout", "archive_keep", "hook_wait", "hook_max_continues", "feedback_url", "feedback_key"];

pub fn load(root: &Path) -> Config {
    fs::read_to_string(paths::config_path(root))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(root: &Path, cfg: &Config) -> Result<()> {
    let path = paths::config_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, toml::to_string(cfg)?).with_context(|| format!("writing {}", path.display()))
}

pub fn set(root: &Path, key: &str, value: &str) -> Result<()> {
    let mut cfg = load(root);
    match key {
        "name" => cfg.name = Some(value.to_string()),
        "agents" => cfg.agents = Some(parse_agents(value)),
        "tail" => cfg.tail = Some(value.parse().context("tail must be a number")?),
        "wait_timeout" => {
            cfg.wait_timeout = Some(value.parse().context("wait_timeout must be seconds")?)
        }
        "archive_keep" => {
            cfg.archive_keep = Some(value.parse().context("archive_keep must be a number")?)
        }
        "hook_wait" => cfg.hook_wait = Some(value.parse().context("hook_wait must be seconds")?),
        "hook_max_continues" => {
            cfg.hook_max_continues = Some(value.parse().context("hook_max_continues must be a number")?)
        }
        "feedback_url" => cfg.feedback_url = Some(value.trim().to_string()),
        "feedback_key" => cfg.feedback_key = Some(value.trim().to_string()),
        _ => bail!("unknown key `{key}`; valid keys: {}", KEYS.join(", ")),
    }
    save(root, &cfg)
}

pub fn get(root: &Path, key: &str) -> Result<Option<String>> {
    let cfg = load(root);
    Ok(match key {
        "name" => cfg.name,
        "agents" => cfg.agents.map(|a| a.join(",")),
        "tail" => cfg.tail.map(|v| v.to_string()),
        "wait_timeout" => cfg.wait_timeout.map(|v| v.to_string()),
        "archive_keep" => cfg.archive_keep.map(|v| v.to_string()),
        "hook_wait" => cfg.hook_wait.map(|v| v.to_string()),
        "hook_max_continues" => cfg.hook_max_continues.map(|v| v.to_string()),
        "feedback_url" => cfg.feedback_url,
        "feedback_key" => cfg.feedback_key,
        _ => bail!("unknown key `{key}`; valid keys: {}", KEYS.join(", ")),
    })
}

pub fn parse_agents(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}
