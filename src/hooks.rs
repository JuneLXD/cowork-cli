//! Editor hooks for Claude Code (.claude/settings.json), Codex (.codex/hooks.json),
//! and Kimi Code (its global config.toml). All three send JSON on stdin. Claude Code
//! and Codex read JSON on stdout; Kimi Code (`--format kimi`) reads exit code 2 plus
//! stderr to continue a turn, and plain stdout as added context.
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

const MARK: &str = "cowork hook ";
/// The command prefix written before the tool was renamed; still recognised so
/// install and remove replace those entries instead of leaving duplicates.
const LEGACY_MARK: &str = "room hook ";

// ---------- unread across rooms ----------

pub struct Unread {
    pub room: String,
    pub messages: Vec<Message>,
    /// Cursor token of the last message in the room, used to advance the cursor.
    pub last_token: String,
}

/// Unread messages from other agents in every room of the project that `me`
/// belongs to. Rooms without a cursor contribute their last few messages from others.
pub fn collect_unread(root: &Path, me: &str) -> Result<Vec<Unread>> {
    let pool = agent::participants(&config::load(root));
    let mut out = Vec::new();
    for name in commands::room_names(root)? {
        let rf = room::parse(&room::read_locked(&paths::room_path(root, &name))?);
        if !rf.participants(&pool).iter().any(|a| a == me) {
            continue;
        }
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

/// Whether `me` belongs to any room of the project. A Kimi hook is global, so it
/// also fires in projects where kimi takes no part; it must not wait there.
fn in_any_room(root: &Path, me: &str) -> Result<bool> {
    for name in commands::room_names(root)? {
        if commands::room_members(root, &name)?.iter().any(|a| a == me) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Returns the process exit code: 2 hands a Kimi Code agent the message.
pub fn stop(agent_flag: Option<&str>, timeout: Option<u64>, format: &str) -> Result<i32> {
    let input = read_stdin_json();
    let Some((project, me)) = context(&input, agent_flag)? else { return Ok(0) };
    if !in_any_room(&project.root, &me)? {
        return Ok(0);
    }
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
        return Ok(0);
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
            let reason = stop_reason(&project, &unread, &me, format);
            advance(&project.root, &me, &unread)?;
            if format == "kimi" {
                eprintln!("{reason}");
                return Ok(2);
            }
            emit(json!({"decision": "block", "reason": reason}));
            return Ok(0);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(0);
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

/// What a Stop hook tells the agent: the messages, one valid command per room, and
/// what happens next. Kimi Code continues a turn once, so kimi is told to wait itself.
fn stop_reason(project: &Project, unread: &[Unread], me: &str, format: &str) -> String {
    let kimi = format == "kimi";
    let prefix = if kimi { format!("COWORK_AGENT={me} ") } else { String::new() };
    let post = |room: &str| {
        format!("`{prefix}cowork post --room {room} --thoughts \"...\" [--action ...] [--taken ...] [--handoff ...] [--vote \"approve: ...\"|\"reject: ...\"]`")
    };
    let answer = match unread {
        [one] => format!("answer in the same room with {}.", post(&one.room)),
        _ => {
            let lines: Vec<String> = unread.iter().map(|u| format!("- room {}: {}", u.room, post(&u.room))).collect();
            format!("answer each message in the room it came from:\n{}\n", lines.join("\n"))
        }
    };
    let next = if kimi {
        let scope = match unread {
            [one] => format!("--room {}", one.room),
            _ => "--all-rooms".to_string(),
        };
        format!(
            "This hook hands you messages only once per turn, so after handling them keep waiting yourself: start \
             `COWORK_AGENT={me} cowork wait {scope} --timeout 600` with run_in_background=true, disable_timeout=true and a description, \
             then call WaitFor on that task."
        )
    } else {
        "When nothing is pending, you may stop; the room keeps listening for you.".to_string()
    };
    format!(
        "New room messages arrived for you (`{me}`). Handle them before stopping.\n\n{}\
         What to do now: read the files they mention if needed, then {answer} \
         If a plan was proposed to you, vote on it. If work was requested and you are the executor, do it and report in --taken. {next}",
        render_unread(project, unread)
    )
}

pub fn prompt(agent_flag: Option<&str>, format: &str) -> Result<()> {
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
    if format == "kimi" {
        println!("{text}");
    } else {
        emit(json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": text}}));
    }
    Ok(())
}

pub fn session(agent_flag: Option<&str>) -> Result<()> {
    let input = read_stdin_json();
    let Some((project, me)) = context(&input, agent_flag)? else { return Ok(()) };
    let cfg = config::load(&project.root);
    let parts = agent::participants(&cfg);
    let other = agent::counterpart(&me, &parts);
    let mut lines = vec![format!(
        "cowork: you are `{me}` in project `{}`, coordinating with `{other}` through the `cowork` CLI. Hooks deliver new room messages to you automatically when you finish a turn and before each user prompt. Rules: `{}`.",
        project.name,
        crate::templates::rules_source(&project.root, &me)
    )];
    let unread = collect_unread(&project.root, &me)?;
    for name in commands::room_names(&project.root)? {
        let rf = room::parse(&room::read_locked(&paths::room_path(&project.root, &name))?);
        if !rf.participants(&parts).iter().any(|a| a == &me) {
            continue;
        }
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
        "Start with `cowork read --room <room>` for any room with unread messages. Role prompt: `cowork prompt {me} --room <room>`."
    ));
    emit(json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": lines.join("\n")}}));
    Ok(())
}

// ---------- install / remove / status ----------

pub fn hook_file(root: &Path, tool: &str) -> PathBuf {
    match tool {
        "claude" => root.join(".claude").join("settings.json"),
        "kimi" => kimi_config_path(),
        _ => root.join(".codex").join("hooks.json"),
    }
}

/// Tools whose hooks live in the project. Kimi Code's live in its global config.
fn is_project_tool(a: &str) -> bool {
    a == "claude" || a == "codex"
}

/// `all` means the project-level tools. Kimi's file is global and shared by every
/// project, so it is touched only when named.
fn tools_for(root: &Path, tool: Option<&str>) -> Result<Vec<String>> {
    let pool = agent::participants(&config::load(root));
    Ok(match tool {
        Some("all") => vec!["claude".into(), "codex".into()],
        Some(t) if is_project_tool(t) || t == "kimi" => vec![t.to_string()],
        Some(t) => bail!("unknown tool `{t}`: use claude, codex, kimi, or all"),
        None => commands::everyone(root, &pool)?.into_iter().filter(|a| is_project_tool(a)).collect(),
    })
}

/// What to run so that each of `agents` gets room messages delivered: project hooks
/// that are missing, and Kimi's global hook, which cowork never installs on its own.
pub fn setup_hints(root: &Path, agents: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for a in agents {
        if installed(root, a) {
            continue;
        }
        if is_project_tool(a) {
            out.push(format!("room hooks for {a} are not installed; run `cowork hook install --tool {a}`"));
        } else if a == "kimi" {
            out.push(format!(
                "kimi: Kimi Code reads hooks only from its global config; to deliver room messages to it, run `cowork hook install --tool kimi` (writes {})",
                kimi_config_path().display()
            ));
        }
    }
    out
}

fn group(cmd: &str, timeout: u64) -> Value {
    json!({"hooks": [{"type": "command", "command": cmd, "timeout": timeout}]})
}

fn is_our_entry(h: &Value) -> bool {
    h["command"]
        .as_str()
        .map(|c| c.starts_with(MARK) || c.starts_with(LEGACY_MARK))
        .unwrap_or(false)
}

fn is_ours(group: &Value) -> bool {
    group["hooks"].as_array().map(|hs| hs.iter().any(is_our_entry)).unwrap_or(false)
}

/// Remove our entries from every group of an event's array, keeping unrelated
/// entries and each group's other keys (matchers and the like). A group is
/// dropped only when nothing is left in it.
fn strip_ours(arr: Vec<Value>) -> Vec<Value> {
    arr.into_iter()
        .filter_map(|mut g| {
            let Some(hs) = g["hooks"].as_array().cloned() else { return Some(g) };
            let kept: Vec<Value> = hs.into_iter().filter(|h| !is_our_entry(h)).collect();
            if kept.is_empty() {
                None
            } else {
                g["hooks"] = Value::Array(kept);
                Some(g)
            }
        })
        .collect()
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
        let mut arr: Vec<Value> = strip_ours(hooks[event].as_array().cloned().unwrap_or_default());
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
        let arr: Vec<Value> = strip_ours(hooks[event.as_str()].as_array().cloned().unwrap_or_default());
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
    if tool == "kimi" {
        return kimi_installed();
    }
    load_json(&hook_file(root, tool))
        .ok()
        .and_then(|v| v["hooks"]["Stop"].as_array().map(|a| a.iter().any(is_ours)))
        .unwrap_or(false)
}

// ---------- kimi: a marked block in the global config.toml ----------

const KIMI_START: &str = "# cowork:start";
const KIMI_END: &str = "# cowork:end";
/// Kimi Code rejects a config whose `[[hooks]]` entries carry any other field.
const KIMI_HOOK_KEYS: &[&str] = &["event", "matcher", "command", "timeout"];

pub fn kimi_config_path() -> PathBuf {
    let home = std::env::var_os("KIMI_CODE_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home_dir().join(".kimi-code"));
    home.join("config.toml")
}

/// The managed block. `pad` is how many newlines install put before it to separate it
/// from the user's text; the start line records it so remove takes back exactly those.
fn kimi_block(wait: u64, pad: usize) -> String {
    format!(
        "{KIMI_START} pad={pad} (managed by cowork; `cowork hook remove --tool kimi` removes this block)\n\
         [[hooks]]\n\
         event = \"Stop\"\n\
         command = \"{MARK}stop --agent kimi --format kimi --timeout {wait}\"\n\
         timeout = {}\n\
         \n\
         [[hooks]]\n\
         event = \"UserPromptSubmit\"\n\
         command = \"{MARK}prompt --agent kimi --format kimi\"\n\
         timeout = 20\n\
         {KIMI_END}\n",
        wait + 30
    )
}

/// Where the block sits: `start` is its first line, `end` the byte after its last,
/// and the `pad` newlines before `start` are ours too.
struct KimiBlock {
    start: usize,
    end: usize,
    pad: usize,
}

fn parse_toml(text: &str) -> Result<toml::Table> {
    text.parse::<toml::Table>().map_err(|e| anyhow::anyhow!("not valid TOML: {}", e.message()))
}

/// The `[[hooks]]` entries of a parsed config, which must be an array of tables.
fn kimi_hooks(cfg: &toml::Table) -> Result<Vec<&toml::Table>> {
    match cfg.get("hooks") {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(a)) => a
            .iter()
            .map(|v| v.as_table().ok_or_else(|| anyhow::anyhow!("`hooks` holds something other than tables")))
            .collect(),
        Some(_) => bail!("`hooks` is not an array of tables"),
    }
}

fn is_our_kimi_hook(h: &toml::Table) -> bool {
    h.get("command").and_then(|c| c.as_str()).map(|c| c.starts_with(MARK) || c.starts_with(LEGACY_MARK)).unwrap_or(false)
}

/// Exactly our Stop and UserPromptSubmit entries, with only the keys Kimi Code allows.
fn check_our_entries(hooks: &[&toml::Table]) -> Result<()> {
    let events: Vec<&str> = hooks.iter().filter_map(|h| h.get("event").and_then(|e| e.as_str())).collect();
    let clean = hooks.iter().all(|h| is_our_kimi_hook(h) && h.keys().all(|k| KIMI_HOOK_KEYS.contains(&k.as_str())));
    if events != ["Stop", "UserPromptSubmit"] || !clean {
        bail!("the cowork block does not hold exactly its Stop and UserPromptSubmit entries; fix or delete it by hand");
    }
    Ok(())
}

/// Find the cowork block and prove it is ours before anything relies on it: one
/// start and one end line in that order, `pad` newlines before it, a body that is
/// exactly our two entries, and a file whose own parse holds those two entries and
/// no other cowork entry. A marker inside a multi-line string, a hand-edited block,
/// or a stray cowork entry all fail here, so nothing is edited on a guess.
fn find_kimi_block(text: &str) -> Result<Option<KimiBlock>> {
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let t = line.trim_end();
        if t.starts_with(KIMI_START) {
            starts.push((pos, t));
        } else if t == KIMI_END {
            ends.push(pos + line.len());
        }
        pos += line.len();
    }
    let full = parse_toml(text)?;
    let ours_in_file: Vec<&toml::Table> = kimi_hooks(&full)?.into_iter().filter(|h| is_our_kimi_hook(h)).collect();
    let block = match (starts.as_slice(), ends.as_slice()) {
        ([], []) => None,
        ([(s, line)], [e]) if s < e => {
            let pad = line[KIMI_START.len()..]
                .trim_start()
                .strip_prefix("pad=")
                .and_then(|r| r.split_whitespace().next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            if pad > *s || !text[s - pad..*s].bytes().all(|b| b == b'\n') {
                bail!("the cowork block's recorded separator does not match the file; fix or delete it by hand");
            }
            let body = parse_toml(&text[*s..*e])?;
            if body.keys().any(|k| k != "hooks") {
                bail!("the cowork block holds more than [[hooks]] entries; fix or delete it by hand");
            }
            check_our_entries(&kimi_hooks(&body)?)?;
            Some(KimiBlock { start: *s, end: *e, pad })
        }
        _ => bail!(
            "found {} `{KIMI_START}` and {} `{KIMI_END}` lines; expected one of each in that order. Fix the file by hand",
            starts.len(),
            ends.len()
        ),
    };
    match &block {
        Some(_) => check_our_entries(&ours_in_file)?,
        None if !ours_in_file.is_empty() => bail!("it has `cowork hook` entries outside a cowork block; remove them by hand"),
        None => {}
    }
    Ok(block)
}

/// Read the config; a missing file is empty, any other read error stops us.
fn read_kimi_config(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(anyhow::anyhow!("cannot read {}: {e}", path.display())),
    }
}

fn kimi_installed() -> bool {
    fs::read_to_string(kimi_config_path())
        .ok()
        .and_then(|t| find_kimi_block(&t).ok().flatten())
        .is_some()
}

pub fn install_kimi(root: &Path) -> Result<PathBuf> {
    let wait = config::load(root).hook_wait.unwrap_or(120).min(570);
    let path = kimi_config_path();
    let ctx = || format!("{} left unchanged", path.display());
    let text = read_kimi_config(&path)?;
    let new = match find_kimi_block(&text).with_context(ctx)? {
        Some(b) => format!("{}{}{}", &text[..b.start], kimi_block(wait, b.pad), &text[b.end..]),
        None => {
            let pad = if text.is_empty() {
                0
            } else if text.ends_with('\n') {
                1
            } else {
                2
            };
            format!("{text}{}{}", "\n".repeat(pad), kimi_block(wait, pad))
        }
    };
    // The whole result must still be a config Kimi Code loads, holding our block.
    if find_kimi_block(&new).with_context(ctx)?.is_none() {
        bail!("{}: the cowork block did not come out as expected", ctx());
    }
    if new != text {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, new).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}

pub fn remove_kimi() -> Result<Option<PathBuf>> {
    let path = kimi_config_path();
    let text = read_kimi_config(&path)?;
    let Some(b) = find_kimi_block(&text).with_context(|| format!("{} left unchanged", path.display()))? else {
        return Ok(None);
    };
    let rest = format!("{}{}", &text[..b.start - b.pad], &text[b.end..]);
    fs::write(&path, rest).with_context(|| format!("writing {}", path.display()))?;
    Ok(Some(path))
}

pub fn install(tool: Option<&str>) -> Result<()> {
    let project = paths::current_project()?;
    paths::ensure_initialized(&project)?;
    let tools = tools_for(&project.root, tool)?;
    if tools.is_empty() {
        println!("no claude or codex agent in this project or its rooms; nothing to install in the project");
    }
    for t in &tools {
        let path = if t == "kimi" { install_kimi(&project.root)? } else { install_for(&project.root, t, t)? };
        println!("installed room hooks for {t} in {}", rel(&project.root, &path));
    }
    print_trust_notes(&tools);
    if !tools.iter().any(|t| t == "kimi") {
        let pool = agent::participants(&config::load(&project.root));
        let all = commands::everyone(&project.root, &pool)?;
        if all.iter().any(|a| a == "kimi") {
            for h in setup_hints(&project.root, &["kimi".to_string()]) {
                println!("  {h}");
            }
        }
    }
    Ok(())
}

pub fn print_trust_notes(tools: &[String]) {
    if tools.iter().any(|t| t == "codex") {
        println!("  codex: hooks run only after you trust them once. In Codex, type /hooks and trust the three `cowork hook` entries.");
    }
    if tools.iter().any(|t| t == "claude") {
        println!("  claude: project hooks load on the next session start; run /hooks inside Claude Code to see them.");
    }
    if tools.iter().any(|t| t == "kimi") {
        println!("  kimi: the hooks are global and fire in every project; outside rooms that include kimi they return at once. They load when a Kimi Code session starts.");
    }
}

pub fn remove(tool: Option<&str>) -> Result<()> {
    let project = paths::current_project()?;
    for t in tools_for(&project.root, tool.or(Some("all")))? {
        let removed = if t == "kimi" { remove_kimi()? } else { remove_for(&project.root, &t)? };
        match removed {
            Some(p) => println!("removed room hooks for {t} from {}", rel(&project.root, &p)),
            None => println!("no room hooks for {t}"),
        }
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let project = paths::current_project()?;
    for t in ["claude", "codex", "kimi"] {
        let p = hook_file(&project.root, t);
        let state = if installed(&project.root, t) { "installed" } else if p.exists() { "file exists, no room hooks" } else { "not installed" };
        let scope = if t == "kimi" { " (global)" } else { "" };
        println!("{t:<7} {state:<28} {}{scope}", rel(&project.root, &p));
    }
    Ok(())
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string())
}
