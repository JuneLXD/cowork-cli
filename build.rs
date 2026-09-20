//! Bakes the shared feedback endpoint into the binary.
//!
//! Sources, in order: the COWORK_FEEDBACK_URL / COWORK_FEEDBACK_KEY pair in the build
//! environment (ROOM_FEEDBACK_URL / ROOM_FEEDBACK_KEY as a legacy pair, never mixed),
//! else `project_ID` and `publishable_key` from a `.env`
//! file next to Cargo.toml. Only those two keys are ever read; secret_key and
//! service_role are ignored. Without either source the build still succeeds and
//! `cowork feedback` explains that no endpoint is configured.

use std::collections::HashMap;
use std::env;
use std::fs;

#[path = "src/feedback_env.rs"]
mod feedback_env;

fn main() {
    println!("cargo:rerun-if-changed=.env");
    for v in ["COWORK_FEEDBACK_URL", "COWORK_FEEDBACK_KEY", "ROOM_FEEDBACK_URL", "ROOM_FEEDBACK_KEY"] {
        println!("cargo:rerun-if-env-changed={v}");
    }

    let dotenv: HashMap<String, String> = fs::read_to_string(".env")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect();

    let var = |name: &str| env::var(name).ok();
    let resolved = feedback_env::resolve_pair(
        (var("COWORK_FEEDBACK_URL"), var("COWORK_FEEDBACK_KEY")),
        (var("ROOM_FEEDBACK_URL"), var("ROOM_FEEDBACK_KEY")),
    )
    .unwrap_or_else(|e| panic!("feedback endpoint in the build environment: {e}"));
    let (url, key) = match resolved {
        Some((u, k, _)) => (u, k),
        None => (
            dotenv
                .get("project_ID")
                .filter(|s| !s.is_empty())
                .map(|id| format!("https://{id}.supabase.co"))
                .unwrap_or_default(),
            dotenv.get("publishable_key").cloned().unwrap_or_default(),
        ),
    };
    // A publishable key is safe to ship; anything else is refused so a mistake in
    // .env cannot leak a server-side credential into the binary.
    let key = if key.is_empty() || key.starts_with("sb_publishable_") { key } else { String::new() };
    println!("cargo:rustc-env=COWORK_FEEDBACK_URL={url}");
    println!("cargo:rustc-env=COWORK_FEEDBACK_KEY={key}");
}
