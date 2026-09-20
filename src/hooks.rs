//! Editor hooks for Claude Code (.claude/settings.json) and Codex (.codex/hooks.json).
//! Both tools share the same hook contract: JSON on stdin, JSON on stdout.
//!
//! - Stop: wait for new room messages and hand them to the agent instead of letting it stop.
//! - UserPromptSubmit: inject unread messages as context before the agent handles a prompt.
//! - SessionStart: brief the agent on its identity, rooms, roles, and open handoffs.

use crate::paths::{self, Project};
use crate::room::Message;
use crate::{agent, commands, config, cursor, daemon, room};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MARK: &str = "room hook ";

// ---------- unread across rooms ----------

pub struct Unread {
    pub room: String,
    pub messages: Vec<Message>,
    /// Cursor token of the last message in the room, used to advance the cursor.
    pub last_token: String,
}

/// Unread messages from other agents in every room of the project.
/// Rooms without a cursor contribute their last few messages from others.
pub fn collect_unread(root: &Path, me: &str) -> Result<Vec<Unread>> {
    let mut out = Vec::new();
    for name in commands::room_names(root)? {
        let rf = room::parse(&room::read_locked(&paths::room_path(root, &name))?);
        let Some(last) = rf.messages.last() else { continue };
        let cur = cursor::load(root, me, &name);
        let msgs: Vec<Message> = match cursor::unread_index(&rf.messages, cur.as_deref()) {
            Some(i) => rf.messages[i..].iter().filter(|m| m.agent != me).cloned().collect(),
            None => {
                let others: Vec<&Message> = rf.messages.iter().filter(|m| m.agent != me).collect();
                let start = others.len().saturating_sub(5);
                others[start..].iter().map(|m| (*m).clone()).collect()
            }
        };
        if !msgs.is_empty() {
            out.push(Unread { room: name, messages: msgs, last_token: last.cursor_token() });
        }
    }
    Ok(out)
}

pub fn advance(root: &Path, me: &str, unread: &[Unread]) -> Result<()> {
    for u in unread {
        cursor::save(root, me, &u.room, &u.last_token)?;
    }
    Ok(())
}

fn render_unread(project: &Project, unread: &[Unread]) -> String {
    let mut s = String::new();
    for u in unread {
        s.push_str(&format!("## {}/{}\n\n", project.name, u.room));
        for m in &u.messages {
            s.push_str(&m.render_display());
        }
    }
    s
}

// ---------- hook handlers ----------

fn read_stdin_json() -> Value {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    serde_json::from_str(&buf).unwrap_or(Value::Null)
}

/// Resolve the project from the hook's cwd, and the agent identity.
fn context(input: &Value, agent_flag: Option<&str>) -> Result<Option<(Project, String)>> {
    if let Some(cwd) = input["cwd"].as_str() {
        let _ = std::env::set_current_dir(cwd);
    }
    let Ok(project) = paths::current_project() else { return Ok(None) };
    if !paths::is_initialized(&project.root) {
        return Ok(None);
    }
    let me = match agent_flag {
        Some(a) => a.to_string(),
        None => match agent::detect_env() {
            Some((a, _)) => a,
            None => return Ok(None),
        },
    };
    Ok(Some((project, me)))
}

fn emit(v: Value) {
    println!("{v}");
}

pub fn stop(agent_flag: Option<&str>, timeout: Option<u64>) -> Result<()> {
    let input = read_stdin_json();
    let Some((project, me)) = context(&input, agent_flag)? else { return Ok(()) };
    let cfg = config::load(&project.root);
    let wait = timeout.or(cfg.hook_wait).unwrap_or(120);
    let max = cfg.hook_max_continues.unwrap_or(100);

    // Bound how many times one session can be kept alive by this hook.
    let session = input["session_id"].as_str().unwrap_or("default").to_string();
    let counter = paths::cursors_dir(&project.root)
        .join(&me)
        .join(format!(".continues-{session}"));
    let count: u64 = fs::read_to_string(&counter)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if input["stop_hook_active"].as_bool().unwrap_or(false) && count >= max {
        return Ok(());
    }

    let rooms_dir = paths::rooms_dir(&project.root);
    let deadline = Instant::now() + Duration::from_secs(wait);
    let mut use_daemon = true;
    loop {
        let unread = collect_unread(&project.root, &me)?;
        if !unread.is_empty() {
            if let Some(parent) = counter.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&counter, (count + 1).to_string())?;
            let rooms: Vec<String> = unread.iter().map(|u| u.room.clone()).collect();
            let reason = format!(
                "New room messages arrived for you (`{me}`). Handle them before stopping.\n\n{}\
                 What to do now: read the files they mention if needed, then answer in the same room with \
                 `room post --room {} --thoughts \"...\" [--action ...] [--taken ...] [--handoff ...] [--vote \"approve: ...\"|\"reject: ...\"]`. \
                 If a plan was proposed to you, vote on it. If work was requested and you are the executor, do it and report in --taken. \
                 When nothing is pending, you may stop; the room keeps listening for you.",
                render_unread(&project, &unread),
                rooms.join("|")
            );
            advance(&project.root, &me, &unread)?;
            emit(json!({"decision": "block", "reason": reason}));
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        let remaining = deadline - now;
        if use_daemon {
            if daemon::watch_change(&rooms_dir, remaining.min(Duration::from_secs(60))).is_err() {
                use_daemon = false;
            }
        } else {
            std::thread::sleep(remaining.min(Duration::from_secs(1)));
        }
    }
}

pub fn prompt(agent_flag: Option<&str>) -> Result<()> {
    let input = read_stdin_json();
    let Some((project, me)) = context(&input, agent_flag)? else { return Ok(()) };
    let unread = collect_unread(&project.root, &me)?;
    if unread.is_empty() {
        return Ok(());
    }
    let text = format!(
        "Unread room messages for `{me}` (already marked as read; act on anything addressed to you, then continue with the user's request):\n\n{}",
        render_unread(&project, &unread)
    );
    advance(&project.root, &me, &unread)?;
    emit(json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": text}}));
    Ok(())
}

pub fn session(agent_flag: Option<&str>) -> Result<()> {
    let input = read_stdin_json();
    let Some((project, me)) = context(&input, agent_flag)? else { return Ok(()) };
    let cfg = config::load(&project.root);
    let parts = agent::participants(&cfg);
    let other = agent::counterpart(&me, &parts);
    let mut lines = vec![format!(
        "room-cli: you are `{me}` in project `{}`, coordinating with `{other}` through the `room` CLI. Hooks deliver new room messages to you automatically when you finish a turn and before each user prompt. Rules: `{}`.",
        project.name,
        crate::templates::rules_file(&me)
    )];
    let unread = collect_unread(&project.root, &me)?;
    for name in commands::room_names(&project.root)? {
        let rf = room::parse(&room::read_locked(&paths::room_path(&project.root, &name))?);
        let executor = rf.front.as_ref().map(|f| f.executor.clone()).unwrap_or_default();
        let role = if executor.is_empty() {
            "no roles".to_string()
        } else if executor == me {
            "you are the executor: propose, get a vote, then edit".to_string()
        } else {
            format!("you are an advisor: review and vote, `{executor}` edits")
        };
        let n = unread.iter().find(|u| u.room == name).map(|u| u.messages.len()).unwrap_or(0);
        let open: Vec<String> = rf
            .messages
            .iter()
            .rev()
            .filter(|m| m.agent != me && m.has_handoff())
            .take(1)
            .map(|m| m.handoff.replace('\n', " "))
            .collect();
        let mut l = format!("- room `{name}`: {role}; {n} unread");
        if let Some(h) = open.first() {
            l.push_str(&format!("; latest handoff from others: {h}"));
        }
        lines.push(l);
    }
    lines.push(format!(
        "Start with `room read --room <room>` for any room with unread messages. Role prompt: `room prompt {me} --room <room>`."
    ));
    emit(json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": lines.join("\n")}}));
    Ok(())
}

// ---------- install / remove / status ----------

pub fn hook_file(root: &Path, tool: &str) -> PathBuf {
    match tool {
        "claude" => root.join(".claude").join("settings.json"),
        _ => root.join(".codex").join("hooks.json"),
    }
}

fn tools_for(root: &Path, tool: Option<&str>) -> Result<Vec<String>> {
    let cfg = config::load(root);
    let parts = agent::participants(&cfg);
    Ok(match tool {
        Some("all") => vec!["claude".into(), "codex".into()],
        Some(t) if t == "claude" || t == "codex" => vec![t.to_string()],
        Some(t) => bail!("unknown tool `{t}`: use claude, codex, or all"),
        None => parts
            .iter()
            .filter(|a| a.as_str() == "claude" || a.as_str() == "codex")
            .cloned()
            .collect(),
    })
}

fn group(cmd: &str, timeout: u64) -> Value {
    json!({"hooks": [{"type": "command", "command": cmd, "timeout": timeout}]})
}

fn is_ours(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .map(|hs| hs.iter().any(|h| h["command"].as_str().map(|c| c.starts_with(MARK)).unwrap_or(false)))
        .unwrap_or(false)
}

fn load_json(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).with_context(|| format!("{} is not valid JSON", path.display()))
        }
        _ => Ok(json!({})),
    }
}

fn save_json(path: &Path, v: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(v)?))
        .with_context(|| format!("writing {}", path.display()))
}

/// Add or refresh the room hooks in one tool's config file. Idempotent.
pub fn install_for(root: &Path, tool: &str, agent_name: &str) -> Result<PathBuf> {
    let cfg = config::load(root);
    let wait = cfg.hook_wait.unwrap_or(120);
    let path = hook_file(root, tool);
    let mut v = load_json(&path)?;
    if !v.is_object() {
        bail!("{} is not a JSON object", path.display());
    }
    let hooks = v["hooks"].as_object().cloned().unwrap_or_default();
    let mut hooks = Value::Object(hooks);
    let wanted = [
        ("Stop", format!("{MARK}stop --agent {agent_name}"), wait + 30),
        ("UserPromptSubmit", format!("{MARK}prompt --agent {agent_name}"), 20),
        ("SessionStart", format!("{MARK}session --agent {agent_name}"), 20),
    ];
    for (event, cmd, timeout) in wanted {
        let mut arr: Vec<Value> = hooks[event].as_array().cloned().unwrap_or_default();
        arr.retain(|g| !is_ours(g));
        arr.push(group(&cmd, timeout));
        hooks[event] = Value::Array(arr);
    }
    v["hooks"] = hooks;
    save_json(&path, &v)?;
    Ok(path)
}

pub fn remove_for(root: &Path, tool: &str) -> Result<Option<PathBuf>> {
    let path = hook_file(root, tool);
    if !path.exists() {
        return Ok(None);
    }
    let mut v = load_json(&path)?;
    let Some(hooks) = v["hooks"].as_object().cloned() else { return Ok(None) };
    let mut hooks = Value::Object(hooks);
    let events: Vec<String> = hooks.as_object().unwrap().keys().cloned().collect();
    for event in events {
        let mut arr: Vec<Value> = hooks[event.as_str()].as_array().cloned().unwrap_or_default();
        arr.retain(|g| !is_ours(g));
        if arr.is_empty() {
            hooks.as_object_mut().unwrap().remove(&event);
        } else {
            hooks[event.as_str()] = Value::Array(arr);
        }
    }
    if hooks.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        v.as_object_mut().unwrap().remove("hooks");
    } else {
        v["hooks"] = hooks;
    }
    if v.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        fs::remove_file(&path)?;
    } else {
        save_json(&path, &v)?;
    }
    Ok(Some(path))
}

pub fn installed(root: &Path, tool: &str) -> bool {
    load_json(&hook_file(root, tool))
        .ok()
        .and_then(|v| v["hooks"]["Stop"].as_array().map(|a| a.iter().any(is_ours)))
        .unwrap_or(false)
}

pub fn install(tool: Option<&str>) -> Result<()> {
    let project = paths::current_project()?;
    paths::ensure_initialized(&project)?;
    let tools = tools_for(&project.root, tool)?;
    if tools.is_empty() {
        println!("no claude or codex participant configured; nothing to install");
        return Ok(());
    }
    for t in &tools {
        let path = install_for(&project.root, t, t)?;
        println!("installed room hooks for {t} in {}", rel(&project.root, &path));
    }
    print_trust_notes(&tools);
    Ok(())
}

pub fn print_trust_notes(tools: &[String]) {
    if tools.iter().any(|t| t == "codex") {
        println!("  codex: hooks run only after you trust them once. In Codex, type /hooks and trust the three `room hook` entries.");
    }
    if tools.iter().any(|t| t == "claude") {
        println!("  claude: project hooks load on the next session start; run /hooks inside Claude Code to see them.");
    }
}

pub fn remove(tool: Option<&str>) -> Result<()> {
    let project = paths::current_project()?;
    for t in tools_for(&project.root, tool.or(Some("all")))? {
        match remove_for(&project.root, &t)? {
            Some(p) => println!("removed room hooks for {t} from {}", rel(&project.root, &p)),
            None => println!("no room hooks for {t}"),
        }
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let project = paths::current_project()?;
    for t in ["claude", "codex"] {
        let p = hook_file(&project.root, t);
        let state = if installed(&project.root, t) { "installed" } else if p.exists() { "file exists, no room hooks" } else { "not installed" };
        println!("{t:<7} {state:<28} {}", rel(&project.root, &p));
    }
    Ok(())
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string())
}
