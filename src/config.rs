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
}

pub const KEYS: &[&str] = &["name", "agents", "tail", "wait_timeout", "archive_keep"];

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
