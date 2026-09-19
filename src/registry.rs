use crate::paths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub projects: BTreeMap<String, String>,
}

pub fn load() -> Registry {
    fs::read_to_string(paths::registry_path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(reg: &Registry) -> Result<()> {
    let path = paths::registry_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, toml::to_string(reg)?).with_context(|| format!("writing {}", path.display()))
}

pub fn register(name: &str, root: &Path) -> Result<()> {
    let mut reg = load();
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    reg.projects
        .insert(name.to_string(), root.to_string_lossy().to_string());
    save(&reg)
}

pub fn lookup(name: &str) -> Option<PathBuf> {
    load().projects.get(name).map(PathBuf::from)
}

/// Registered projects whose directory still exists, name -> root.
pub fn live_projects() -> Vec<(String, PathBuf)> {
    load()
        .projects
        .into_iter()
        .map(|(n, p)| (n, PathBuf::from(p)))
        .filter(|(_, p)| p.is_dir())
        .collect()
}
