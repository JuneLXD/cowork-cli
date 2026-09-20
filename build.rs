//! Bakes the shared feedback endpoint into the binary.
//!
//! Sources, in order: the ROOM_FEEDBACK_URL / ROOM_FEEDBACK_KEY environment
//! variables at build time, else `project_ID` and `publishable_key` from a `.env`
//! file next to Cargo.toml. Only those two keys are ever read; secret_key and
//! service_role are ignored. Without either source the build still succeeds and
//! `room feedback` explains that no endpoint is configured.

use std::collections::HashMap;
use std::env;
use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=.env");
    println!("cargo:rerun-if-env-changed=ROOM_FEEDBACK_URL");
    println!("cargo:rerun-if-env-changed=ROOM_FEEDBACK_KEY");

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

    let url = env::var("ROOM_FEEDBACK_URL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
        dotenv
            .get("project_ID")
            .filter(|s| !s.is_empty())
            .map(|id| format!("https://{id}.supabase.co"))
            .unwrap_or_default()
    });
    let key = env::var("ROOM_FEEDBACK_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| dotenv.get("publishable_key").cloned())
        .unwrap_or_default();
    // A publishable key is safe to ship; anything else is refused so a mistake in
    // .env cannot leak a server-side credential into the binary.
    let key = if key.is_empty() || key.starts_with("sb_publishable_") { key } else { String::new() };
    println!("cargo:rustc-env=ROOM_FEEDBACK_URL={url}");
    println!("cargo:rustc-env=ROOM_FEEDBACK_KEY={key}");
}
