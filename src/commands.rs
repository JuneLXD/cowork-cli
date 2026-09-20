use crate::cli::{ConfigCmd, DaemonCmd, PostArgs, ReadArgs, WaitArgs};
use crate::paths::{self, Project, RoomRef};
use crate::room::{self, FrontMatter, Message, NONE};
use crate::{agent, config, cursor, daemon, registry, templates};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use fs2::FileExt;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ---------- init ----------

pub fn init(agents: Option<String>, no_git: bool, quiet: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = match paths::find_git_root(&cwd) {
        Some(r) => r,
        None if no_git => cwd.clone(),
        None if crate::menu::interactive()
            && crate::menu::confirm(
                &format!("No git repository found above {}. Initialize here anyway?", cwd.display()),
                false,
            )? =>
        {
            cwd.clone()
        }
        None => bail!(
            "{} is not inside a git repository. Run `git init` first, or pass --no-git.",
            cwd.display()
        ),
    };
    if let Some(a) = agents {
        let list = config::parse_agents(&a);
        if list.len() < 2 {
            bail!("--agents needs at least two names, e.g. --agents claude,codex");
        }
        for n in &list {
            room::validate_agent(n)?;
        }
        config::set(&root, "agents", &a)?;
    }
    let project = paths::project_at(root.clone());
    let cfg = config::load(&root);
    let participants = agent::participants(&cfg);

    fs::create_dir_all(paths::rooms_dir(&root))?;
    fs::create_dir_all(paths::cursors_dir(&root))?;

    let main = paths::room_path(&root, "main");
    let mut created = Vec::new();
    if !main.exists() {
        let fm = FrontMatter {
            room: "main".into(),
            project: project.name.clone(),
            created: room::now_ts(),
            purpose: "General coordination".into(),
            participants: participants.clone(),
            executor: String::new(),
            last_id: 0,
        };
        fs::write(&main, fm.render())?;
        created.push(".ai-common/rooms/main.md");
    }
    // Create-only: people add their own rules to PROTOCOL.md. Delete it to regenerate.
    let proto = paths::protocol_path(&root);
    if !proto.exists() {
        fs::write(&proto, templates::protocol())?;
        created.push(".ai-common/PROTOCOL.md");
    }

    // Rules blocks: one file per tool, shared by every agent that maps to it.
    let mut by_file: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for a in &participants {
        by_file
            .entry(templates::rules_file(a))
            .or_default()
            .push(a.clone());
    }
    let mut updated = Vec::new();
    for (file, agents_here) in &by_file {
        let block = if agents_here.len() == 1 {
            let a = &agents_here[0];
            templates::rules(&root, a, &agent::counterpart(a, &participants), &project.name)
        } else {
            let names = agents_here
                .iter()
                .map(|a| format!("`{a}`"))
                .collect::<Vec<_>>()
                .join(" or ");
            templates::rules(
                &root,
                &format!("{names} (set COWORK_AGENT to your name)"),
                "the other agents",
                &project.name,
            )
        };
        templates::upsert_block(&root.join(file), &block)?;
        updated.push(*file);
    }

    gitignore_add(&root, ".ai-common/.cursors/")?;
    registry::register(&project.name, &root)?;

    // Editor hooks so both tools receive room messages without polling.
    let mut hooked: Vec<String> = Vec::new();
    for a in &participants {
        if a == "claude" || a == "codex" {
            crate::hooks::install_for(&root, a, a)?;
            hooked.push(a.clone());
        }
    }

    // Onboarding prompts.
    let mut onboarding = format!(
        "# cowork onboarding for `{}`\n\nPaste the matching prompt into each tool's session. Regenerate any time with `cowork prompt <agent>`.\n\n",
        project.name
    );
    let mut printed = String::new();
    for a in &participants {
        let other = agent::counterpart(a, &participants);
        let k = templates::kickoff(&root, a, &other, &project.name);
        onboarding.push_str(&format!("## {a}\n\n```\n{k}\n```\n\n"));
        printed.push_str(&format!(
            "── prompt for {a} (paste into its session; rules already in {}) ──\n{k}\n\n",
            templates::rules_file(a)
        ));
    }
    fs::write(paths::onboarding_path(&root), &onboarding)?;

    println!("initialized project `{}` at {}", project.name, root.display());
    if !created.is_empty() {
        println!("  created: {}", created.join(", "));
    }
    println!("  rules blocks: {}", updated.join(", "));
    if !hooked.is_empty() {
        let files: Vec<String> = hooked
            .iter()
            .map(|t| crate::hooks::hook_file(&root, t).strip_prefix(&root).map(|p| p.display().to_string()).unwrap_or_default())
            .collect();
        println!("  hooks: {}", files.join(", "));
        crate::hooks::print_trust_notes(&hooked);
    }
    println!("  prompts saved to .ai-common/ONBOARDING.md");
    if !quiet {
        println!();
        print!("{printed}");
        println!("next: open each tool in this directory, paste its prompt, and let them talk.");
    }
    Ok(())
}

fn gitignore_add(root: &Path, entry: &str) -> Result<()> {
    let p = root.join(".gitignore");
    let existing = fs::read_to_string(&p).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == entry) {
        return Ok(());
    }
    let mut t = existing;
    if !t.is_empty() && !t.ends_with('\n') {
        t.push('\n');
    }
    t.push_str(entry);
    t.push('\n');
    fs::write(&p, t)?;
    Ok(())
}

// ---------- post ----------

fn field(text: Option<String>, file: Option<String>) -> Result<Option<String>> {
    if let Some(f) = file {
        let content = if f == "-" {
            if unsafe { libc::isatty(0) } == 1 {
                eprintln!("(reading from stdin until end of input; pipe the text or use a heredoc)");
            }
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        } else {
            fs::read_to_string(&f).with_context(|| format!("reading {f}"))?
        };
        return Ok(Some(content.trim_end().to_string()));
    }
    Ok(text)
}

/// stdin can feed only one field: a second `-` would read an already-drained
/// stream and silently record `None` (this happened in real runs).
fn check_stdin_once(a: &PostArgs) -> Result<()> {
    let users: Vec<&str> = [
        ("--thoughts-file", &a.thoughts_file),
        ("--action-file", &a.action_file),
        ("--taken-file", &a.taken_file),
        ("--handoff-file", &a.handoff_file),
    ]
    .iter()
    .filter(|(_, v)| v.as_deref() == Some("-"))
    .map(|(n, _)| *n)
    .collect();
    if users.len() > 1 {
        bail!(
            "stdin (-) can be read by only one --*-file flag, but {} all use it; put the other fields in files or pass them inline",
            users.join(", ")
        );
    }
    Ok(())
}

pub fn post(a: PostArgs) -> Result<()> {
    let rr = paths::resolve_room(a.room.as_deref())?;
    paths::ensure_room_exists(&rr)?;
    let me = agent::detect(a.agent.as_deref())?;
    room::validate_agent(&me)?;
    check_stdin_once(&a)?;
    let vote = match a.vote {
        Some(v) => normalize_vote(&v)?,
        None => String::new(),
    };
    let or_none = |v: Option<String>| v.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| NONE.into());
    let taken = or_none(field(a.taken, a.taken_file)?);
    // A vote, a completion, or a taken report is substantive on its own; anything
    // else needs thoughts.
    let thoughts = match field(a.thoughts, a.thoughts_file)?.filter(|s| !s.trim().is_empty()) {
        Some(t) => t,
        None if !vote.is_empty() || a.complete || taken != NONE => NONE.to_string(),
        None => bail!("--thoughts <TEXT> or --thoughts-file <PATH> is required (a post with only --vote, --complete, or --taken may omit it)"),
    };
    if let Some(0) = a.re {
        bail!("--re needs a message id of 1 or more (ids are printed by `cowork post` and shown in `cowork read`)");
    }
    let mut msg = Message {
        id: None,
        re: a.re,
        proposal: a.propose,
        completes: if a.complete { a.re } else { None },
        agent: me.clone(),
        timestamp: room::now_ts(),
        thoughts,
        action: or_none(field(a.action, a.action_file)?),
        taken,
        handoff: or_none(field(a.handoff, a.handoff_file)?),
        vote,
    };
    let addr = rr.addr();
    let cfg = config::load(&rr.project.root);
    let cfg_participants = agent::participants(&cfg);
    let mut participants: Vec<String> = Vec::new();
    let mut note: Option<String> = None;
    let mut unread_behind: Vec<(u64, String)> = Vec::new();
    let my_cursor = cursor::load(&rr.project.root, &me, &rr.room);
    // The id is allocated, and the reply target checked, while the append lock is
    // held, so concurrent posts cannot collide and a bad --re appends nothing.
    room::append_locked(&rr.path, |content| {
        let rf = room::parse(content);
        let last = rf.last_id();
        if let Some(re) = msg.re {
            if re > last {
                bail!("--re {re}: no such message in {addr}; the newest id is {last}");
            }
        }
        participants = rf.participants(&cfg_participants);
        if let Some(idx) = cursor::unread_index(&rf.messages, my_cursor.as_deref()) {
            unread_behind = rf.messages[idx..]
                .iter()
                .filter(|m| m.agent != me)
                .map(|m| (m.id.unwrap_or(0), m.agent.clone()))
                .collect();
        }
        let analysis = crate::state::analyze(&rf, &participants);
        if let Some(c) = msg.completes {
            let Some(p) = analysis.get(c) else {
                bail!("--complete --re {c}: #{c} is not a proposal in {addr} (open proposals: {})", open_ids(&analysis));
            };
            if p.state.is_terminal() {
                bail!("--complete --re {c}: #{c} is already {} ", p.state.label());
            }
            if me != p.author && me != analysis.executor {
                bail!("--complete --re {c}: only {} (its author) or {} (the executor) can complete #{c}", p.author, analysis.executor);
            }
        }
        if let Some(re) = msg.re {
            if let Some(p) = analysis.get(re) {
                if let Some(d) = msg.decision() {
                    if me == p.author {
                        note = Some(format!("note: a vote on your own proposal #{re} is informational; it does not count"));
                    } else if !participants.iter().any(|a| a == &me) {
                        note = Some(format!("note: `{me}` is not a participant of this room, so this vote on #{re} is informational"));
                    } else {
                        note = Some(format!("{d} on #{re}"));
                    }
                }
            } else if !msg.vote.is_empty() {
                note = Some(format!("note: #{re} is not a proposal, so this vote is informational; vote on the proposal id shown by `cowork status`"));
            }
        } else if !msg.vote.is_empty() {
            note = Some("note: this vote is not linked to a proposal; add --re <id> so it counts as the decision on that proposal".to_string());
        }
        msg.id = Some(last + 1);
        msg.timestamp = room::now_ts();
        Ok(msg.render())
    })?;
    let id = msg.id.unwrap_or(0);
    let mut tag = String::new();
    if msg.proposal {
        let others = agent::counterpart(&me, &participants);
        tag = format!(" (proposal; {others} votes with: cowork post --room {} --re {id} --vote \"approve: reason\" [--thoughts \"...\"])", rr.room);
    } else if let Some(c) = msg.completes {
        tag = format!(" (completes #{c})");
    } else if let Some(n) = &note {
        if !n.starts_with("note:") {
            tag = format!(" ({n})");
        }
    }
    if let Some(n) = &note {
        if n.starts_with("note:") {
            eprintln!("{n}");
        }
    }
    if !unread_behind.is_empty() {
        let mut agents: Vec<String> = unread_behind.iter().map(|(_, a)| a.clone()).collect();
        agents.dedup();
        let ids: Vec<String> = unread_behind.iter().filter(|(i, _)| *i > 0).map(|(i, _)| format!("#{i}")).collect();
        let ids = if ids.is_empty() { String::new() } else { format!(" ({})", ids.join(", ")) };
        eprintln!(
            "note: {} unread message(s) from {}{ids} precede this post; run `cowork read --room {}` before acting on their replies",
            unread_behind.len(),
            agents.join(", "),
            rr.room
        );
    }
    // Posting is not reading: never move an existing cursor, or a message that
    // arrived while this post was being written would be skipped. A caller with
    // no cursor gets an empty one, so its first read or wait starts from the
    // beginning instead of from the current tail.
    if cursor::load(&rr.project.root, &me, &rr.room).is_none() {
        cursor::save(&rr.project.root, &me, &rr.room, "")?;
    }
    println!("posted #{id} to {} as {} at {}{tag}", rr.addr(), me, msg.timestamp);
    Ok(())
}

fn open_ids(a: &crate::state::Analysis) -> String {
    let ids: Vec<String> = a.open().map(|p| format!("#{}", p.id)).collect();
    if ids.is_empty() {
        "none".to_string()
    } else {
        ids.join(", ")
    }
}

/// A vote must start with approve, reject, or abstain and a colon, so the decision
/// is machine-readable. The reason after the colon stays free text.
fn normalize_vote(v: &str) -> Result<String> {
    let t = v.trim();
    let (word, rest) = t
        .split_once(':')
        .ok_or_else(|| anyhow!("--vote must look like \"approve: reason\", \"reject: reason\", or \"abstain: reason\" (got `{t}`)"))?;
    let word = word.trim().to_ascii_lowercase();
    if !["approve", "reject", "abstain"].contains(&word.as_str()) {
        bail!("--vote must start with approve, reject, or abstain followed by a colon (got `{t}`)");
    }
    let rest = rest.trim();
    if rest.is_empty() {
        bail!("--vote needs a reason after the colon, e.g. \"{word}: because ...\"");
    }
    Ok(format!("{word}: {rest}"))
}

// ---------- read ----------

fn print_messages(msgs: &[Message], json: bool, empty_note: &str) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(msgs)?);
    } else if msgs.is_empty() {
        println!("{empty_note}");
    } else {
        for m in msgs {
            print!("{}", m.render_display());
        }
    }
    Ok(())
}

/// One line per message for `read --brief`. Proposal and vote markers come from
/// the whole room so a filtered slice still shows the right state.
fn print_brief(all: &room::RoomFile, msgs: &[Message], participants: &[String]) {
    let analysis = crate::state::analyze(all, participants);
    for m in msgs {
        let id = m.id.map(|i| format!("#{i}")).unwrap_or_else(|| "-".to_string());
        let marker = if m.proposal {
            let st = m.id.and_then(|i| analysis.get(i)).map(|p| p.state.label()).unwrap_or("proposal");
            format!("[proposal: {st}]")
        } else if let Some(c) = m.completes {
            format!("[completes #{c}]")
        } else if let Some(d) = m.decision() {
            let counted = m
                .re
                .and_then(|re| analysis.get(re))
                .map(|p| p.votes.iter().any(|v| Some(v.id) == m.id))
                .unwrap_or(false);
            match (counted, m.re) {
                (true, Some(re)) => format!("[{d} on #{re}]"),
                (false, Some(re)) => format!("[{d} on #{re}, informational]"),
                (false, None) => format!("[{d}, informational]"),
                (true, None) => format!("[{d}]"),
            }
        } else if m.re.is_some() {
            format!("[re #{}]", m.re.unwrap())
        } else {
            String::new()
        };
        let is_none = |v: &str| v.trim().is_empty() || v.trim().eq_ignore_ascii_case(NONE);
        let text = if !m.vote.is_empty() {
            &m.vote
        } else if !is_none(&m.action) {
            &m.action
        } else if !is_none(&m.taken) {
            &m.taken
        } else if !is_none(&m.handoff) {
            &m.handoff
        } else {
            &m.thoughts
        };
        let first = text.lines().next().unwrap_or("").trim();
        let mut short: String = first.chars().take(80).collect();
        if first.chars().count() > 80 {
            short.push_str("...");
        }
        let age = crate::state::age(&m.timestamp);
        let marker = if marker.is_empty() { String::new() } else { format!(" {marker}") };
        println!("{id:>5} {:<8} {age:>8}{marker} {short}", m.agent);
    }
}

pub fn read(a: ReadArgs) -> Result<()> {
    let rr = paths::resolve_room(a.room.as_deref())?;
    paths::ensure_room_exists(&rr)?;
    let cfg = config::load(&rr.project.root);
    let content = room::read_locked(&rr.path)?;
    let rf = room::parse(&content);
    let unread_mode = a.tail.is_none() && a.last.is_none() && a.since.is_none();

    let mut msgs: Vec<Message>;
    let mut raw: Option<String> = None;
    if let Some(n) = a.tail {
        raw = Some(room::last_lines(&content, n));
        msgs = room::parse(raw.as_deref().unwrap()).messages;
    } else if let Some(n) = a.last {
        let start = rf.messages.len().saturating_sub(n);
        msgs = rf.messages[start..].to_vec();
    } else if let Some(s) = &a.since {
        msgs = rf
            .messages
            .iter()
            .filter(|m| m.timestamp.as_str() >= s.as_str())
            .cloned()
            .collect();
    } else {
        let me = agent::detect(a.me.as_deref()).map_err(|_| {
            anyhow!("unread mode needs to know who you are: pass --me <NAME>, set COWORK_AGENT, or use --tail/--last/--since")
        })?;
        let cur = cursor::load(&rr.project.root, &me, &rr.room);
        match cursor::unread_index(&rf.messages, cur.as_deref()) {
            Some(i) => msgs = rf.messages[i..].to_vec(),
            None => {
                // First read ever: the most recent complete messages that fit the
                // tail budget, and always at least the latest one. Parsing a raw line
                // slice instead could start inside a long message and drop it.
                let budget = cfg.tail.unwrap_or(40);
                let mut start = rf.messages.len();
                let mut lines = 0;
                while start > 0 {
                    let n = rf.messages[start - 1].render().lines().count();
                    if lines + n > budget && start < rf.messages.len() {
                        break;
                    }
                    lines += n;
                    start -= 1;
                }
                msgs = rf.messages[start..].to_vec();
            }
        }
        // Unread means "what others said": own posts are hidden unless asked for with --agent.
        if a.agent.is_none() {
            msgs.retain(|m| m.agent != me);
        }
        if !a.no_advance {
            if let Some(last) = rf.messages.last() {
                cursor::save(&rr.project.root, &me, &rr.room, &last.cursor_token())?;
            }
        }
    }
    if let Some(f) = &a.agent {
        msgs.retain(|m| &m.agent == f);
        raw = None;
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&msgs)?);
    } else if a.brief {
        if msgs.is_empty() {
            println!("(no {} messages in {})", if unread_mode { "unread" } else { "matching" }, rr.addr());
        } else {
            let participants = rf.participants(&agent::participants(&cfg));
            print_brief(&rf, &msgs, &participants);
        }
    } else if let Some(t) = raw {
        print!("{t}");
    } else {
        let note = if unread_mode {
            format!("(no unread messages in {})", rr.addr())
        } else {
            format!("(no matching messages in {})", rr.addr())
        };
        print_messages(&msgs, false, &note)?;
    }
    Ok(())
}

// ---------- wait ----------

pub fn wait(a: WaitArgs) -> Result<i32> {
    let cfg;
    let (project, rooms, watch_path, label): (Project, Vec<String>, std::path::PathBuf, String) = if a.all_rooms {
        let project = paths::current_project()?;
        paths::ensure_initialized(&project)?;
        cfg = config::load(&project.root);
        let rooms = room_names(&project.root)?;
        let watch = paths::rooms_dir(&project.root);
        let label = format!("{}/*", project.name);
        (project, rooms, watch, label)
    } else {
        let rr = paths::resolve_room(a.room.as_deref())?;
        paths::ensure_room_exists(&rr)?;
        cfg = config::load(&rr.project.root);
        let label = rr.addr();
        (rr.project.clone(), vec![rr.room.clone()], rr.path.clone(), label)
    };
    let me = agent::detect(a.me.as_deref())?;
    let root = project.root.clone();
    let timeout = a.timeout.or(cfg.wait_timeout).unwrap_or(300);

    // No cursor yet: start listening from now.
    for r in &rooms {
        if cursor::load(&root, &me, r).is_none() {
            let rf = room::parse(&room::read_locked(&paths::room_path(&root, r))?);
            let h = rf.messages.last().map(|m| m.cursor_token()).unwrap_or_default();
            cursor::save(&root, &me, r, &h)?;
        }
    }

    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut use_daemon = true;
    loop {
        let mut found = false;
        for r in &rooms {
            let rf = room::parse(&room::read_locked(&paths::room_path(&root, r))?);
            let cur = cursor::load(&root, &me, r);
            let idx = cursor::unread_index(&rf.messages, cur.as_deref()).unwrap_or(rf.messages.len());
            let fresh: Vec<Message> = rf.messages[idx..].iter().filter(|m| m.agent != me).cloned().collect();
            if !fresh.is_empty() {
                if a.all_rooms && !a.json {
                    println!("## {}/{r}", project.name);
                }
                print_messages(&fresh, a.json, "")?;
                if let Some(last) = rf.messages.last() {
                    cursor::save(&root, &me, r, &last.cursor_token())?;
                }
                found = true;
            }
        }
        if found {
            return Ok(0);
        }
        let now = Instant::now();
        if now >= deadline {
            eprintln!("timeout: no new post from others in {label} within {timeout}s");
            return Ok(2);
        }
        let remaining = deadline - now;
        if use_daemon {
            match daemon::watch_change(&watch_path, remaining.min(Duration::from_secs(60))) {
                Ok(_) => continue,
                Err(_) => use_daemon = false,
            }
        }
        std::thread::sleep(remaining.min(Duration::from_secs(1)));
    }
}

// ---------- new / list / status ----------

pub fn new_room(name: &str, purpose: Option<String>, executor: Option<String>) -> Result<()> {
    let project = paths::current_project()?;
    paths::ensure_initialized(&project)?;
    paths::validate_room_name(name)?;
    let path = paths::room_path(&project.root, name);
    if path.exists() {
        bail!("room `{}` already exists in project `{}`", name, project.name);
    }
    let cfg = config::load(&project.root);
    let participants = agent::participants(&cfg);
    let executor = match executor {
        Some(e) => {
            room::validate_agent(&e)?;
            e.trim().to_string()
        }
        None => participants[0].clone(),
    };
    let fm = FrontMatter {
        room: name.to_string(),
        project: project.name.clone(),
        created: room::now_ts(),
        purpose: purpose.unwrap_or_default(),
        participants: participants.clone(),
        executor: executor.clone(),
        last_id: 0,
    };
    fs::write(&path, fm.render())?;
    println!("created room {}/{} (executor: {executor})", project.name, name);
    println!();
    print!("{}", room_prompts_text(&project, name)?);
    println!("saved to .ai-common/prompts/{name}.md; reprint with `cowork prompt <agent> --room {name}`");
    Ok(())
}

/// Remove a room's log, every agent's cursor for it, and its saved prompts.
/// Archives under .ai-common/archive/ are kept.
pub fn delete_room(name: &str, yes: bool) -> Result<()> {
    let project = paths::current_project()?;
    paths::ensure_initialized(&project)?;
    paths::validate_room_name(name)?;
    if name == "main" {
        bail!("`main` is the default room and cannot be deleted; use `cowork archive --room main` to clear it");
    }
    let path = paths::room_path(&project.root, name);
    if !path.exists() {
        bail!("room `{}` does not exist in project `{}`", name, project.name);
    }
    let count = room::parse(&room::read_locked(&path)?).messages.len();
    if !yes {
        if !crate::menu::interactive() {
            bail!("refusing to delete {}/{name} ({count} messages) without --yes", project.name);
        }
        let ok = crate::menu::confirm(
            &format!("Delete {}/{name} and its {count} messages? This cannot be undone.", project.name),
            false,
        )?;
        if !ok {
            println!("kept {}/{name}", project.name);
            return Ok(());
        }
    }
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    let mut extra = 0;
    if let Ok(agents) = fs::read_dir(paths::cursors_dir(&project.root)) {
        for a in agents.flatten() {
            let c = a.path().join(name);
            if c.exists() && fs::remove_file(&c).is_ok() {
                extra += 1;
            }
        }
    }
    let prompts = paths::ai_dir(&project.root).join("prompts").join(format!("{name}.md"));
    if prompts.exists() && fs::remove_file(&prompts).is_ok() {
        extra += 1;
    }
    println!(
        "deleted {}/{name} ({count} messages, {extra} related files). Archives under .ai-common/archive/ were kept.",
        project.name
    );
    Ok(())
}

/// (agent, role, prompt) for every participant of a room.
pub fn room_prompts(project: &Project, name: &str) -> Result<Vec<(String, String, String)>> {
    let path = paths::room_path(&project.root, name);
    if !path.exists() {
        bail!("room `{}` does not exist in project `{}`", name, project.name);
    }
    let rf = room::parse(&room::read_locked(&path)?);
    let cfg = config::load(&project.root);
    let mut participants = rf
        .front
        .as_ref()
        .map(|f| f.participants.clone())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| agent::participants(&cfg));
    let executor = rf
        .front
        .as_ref()
        .map(|f| f.executor.clone())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| participants[0].clone());
    if !participants.iter().any(|p| p == &executor) {
        participants.push(executor.clone());
    }
    let mut out = Vec::new();
    for a in &participants {
        let role = if a == &executor { "executor" } else { "advisor" };
        let other = agent::counterpart(a, &participants);
        let text = templates::role_prompt(&project.root, role, a, &other, &project.name, name);
        out.push((a.clone(), role.to_string(), text));
    }
    Ok(out)
}

/// Render the role prompts for printing and save them under .ai-common/prompts/.
pub fn room_prompts_text(project: &Project, name: &str) -> Result<String> {
    let prompts = room_prompts(project, name)?;
    let mut printed = String::new();
    let mut saved = format!("# Role prompts for `{}/{}`\n\nPaste each prompt into that agent's session.\n\n", project.name, name);
    for (a, role, text) in &prompts {
        let what = if role == "executor" { "reads, suggests, votes, and executes (writes changes)" } else { "reads, suggests, and votes (no file changes)" };
        printed.push_str(&format!("── {role} prompt for {a}: {what} ──\n{text}\n\n"));
        saved.push_str(&format!("## {a} ({role})\n\n```\n{text}\n```\n\n"));
    }
    let dir = paths::ai_dir(&project.root).join("prompts");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(format!("{name}.md")), saved)?;
    Ok(printed)
}

/// Follow a room live until ctrl-c.
pub fn stream(room_addr: Option<String>, last: usize, json: bool) -> Result<()> {
    let rr = paths::resolve_room(room_addr.as_deref())?;
    paths::ensure_room_exists(&rr)?;
    let rf = room::parse(&room::read_locked(&rr.path)?);
    let start = rf.messages.len().saturating_sub(last);
    let mut seen = rf.messages.last().map(|m| m.cursor_token()).unwrap_or_default();
    print_messages(&rf.messages[start..], json, "")?;
    eprintln!("── streaming {} · ctrl-c to stop ──", rr.addr());
    install_sigint();
    let mut use_daemon = true;
    while !STOP.load(std::sync::atomic::Ordering::SeqCst) {
        if use_daemon {
            if daemon::watch_change(&rr.path, Duration::from_secs(1)).is_err() {
                use_daemon = false;
            }
        } else {
            std::thread::sleep(Duration::from_millis(500));
        }
        let rf = room::parse(&room::read_locked(&rr.path)?);
        let idx = cursor::unread_index(&rf.messages, Some(&seen)).unwrap_or(0);
        if idx < rf.messages.len() {
            print_messages(&rf.messages[idx..], json, "")?;
            seen = rf.messages.last().map(|m| m.cursor_token()).unwrap_or_default();
        }
    }
    STOP.store(false, std::sync::atomic::Ordering::SeqCst);
    restore_sigint();
    eprintln!("── stopped ──");
    Ok(())
}

static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    STOP.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn install_sigint() {
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as libc::sighandler_t);
    }
}

fn restore_sigint() {
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }
}

struct RoomRow {
    name: String,
    count: usize,
    last_agent: String,
    last_ts: String,
    purpose: String,
}

pub fn room_names(root: &Path) -> Result<Vec<String>> {
    let dir = paths::rooms_dir(root);
    let mut names: Vec<String> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.extension().map(|x| x == "md").unwrap_or(false) {
                p.file_stem().map(|s| s.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    if let Some(i) = names.iter().position(|n| n == "main") {
        let m = names.remove(i);
        names.insert(0, m);
    }
    Ok(names)
}

fn room_rows(root: &Path) -> Result<Vec<RoomRow>> {
    let mut rows = Vec::new();
    for name in room_names(root)? {
        let rf = room::parse(&room::read_locked(&paths::room_path(root, &name))?);
        let (la, lt) = rf
            .messages
            .last()
            .map(|m| (m.agent.clone(), m.timestamp.clone()))
            .unwrap_or_default();
        rows.push(RoomRow {
            name,
            count: rf.messages.len(),
            last_agent: la,
            last_ts: lt,
            purpose: rf.front.map(|f| f.purpose).unwrap_or_default(),
        });
    }
    Ok(rows)
}

fn print_rows(rows: &[RoomRow]) {
    let w = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    println!("  {:<w$}  {:>5}  {:<8}  {:<23}  {}", "ROOM", "MSGS", "LAST BY", "LAST AT", "PURPOSE", w = w);
    for r in rows {
        println!(
            "  {:<w$}  {:>5}  {:<8}  {:<23}  {}",
            r.name,
            r.count,
            if r.last_agent.is_empty() { "-" } else { &r.last_agent },
            if r.last_ts.is_empty() { "-" } else { &r.last_ts },
            r.purpose,
            w = w
        );
    }
}

pub fn list(all: bool) -> Result<()> {
    if all {
        let projects = registry::live_projects();
        if projects.is_empty() {
            println!("no registered projects yet (run `cowork init` in a repository)");
            return Ok(());
        }
        for (name, root) in projects {
            println!("{name}  ({})", root.display());
            if paths::is_initialized(&root) {
                print_rows(&room_rows(&root)?);
            } else {
                println!("  (not initialized)");
            }
            println!();
        }
    } else {
        let project = paths::current_project()?;
        paths::ensure_initialized(&project)?;
        println!("{}  ({})", project.name, project.root.display());
        print_rows(&room_rows(&project.root)?);
    }
    Ok(())
}

fn room_status(project: &Project, name: &str) -> Result<serde_json::Value> {
    let rf = room::parse(&room::read_locked(&paths::room_path(&project.root, name))?);
    let cfg = config::load(&project.root);
    let participants = rf.participants(&agent::participants(&cfg));
    let analysis = crate::state::analyze(&rf, &participants);
    let mut latest: BTreeMap<String, &Message> = BTreeMap::new();
    for m in &rf.messages {
        latest.insert(m.agent.clone(), m);
    }
    let last_post: BTreeMap<&String, &String> = latest.iter().map(|(a, m)| (a, &m.timestamp)).collect();
    let open: Vec<_> = latest
        .values()
        .filter(|m| m.has_handoff())
        .map(|m| json!({"agent": m.agent, "timestamp": m.timestamp, "handoff": m.handoff}))
        .collect();
    Ok(json!({
        "project": project.name,
        "room": name,
        "messages": rf.messages.len(),
        "executor": analysis.executor,
        "policy": analysis.policy,
        "proposals": analysis.proposals,
        "informational_votes": analysis.informational_votes,
        "last_post": last_post,
        "open_handoffs": open,
    }))
}

fn print_proposal_line(p: &serde_json::Value) {
    let votes: Vec<String> = p["votes"]
        .as_array()
        .map(|vs| vs.iter().map(|v| format!("{} {}", v["agent"].as_str().unwrap_or(""), v["decision"].as_str().unwrap_or(""))).collect())
        .unwrap_or_default();
    let waiting: Vec<&str> = p["waiting_for"].as_array().map(|w| w.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
    let mut extra = Vec::new();
    if !votes.is_empty() {
        extra.push(format!("votes: {}", votes.join(", ")));
    }
    if !waiting.is_empty() {
        extra.push(format!("waiting for: {}", waiting.join(", ")));
    }
    let extra = if extra.is_empty() { String::new() } else { format!("  [{}]", extra.join("; ")) };
    println!(
        "    #{} {} {}: {}{extra}",
        p["id"],
        p["author"].as_str().unwrap_or(""),
        crate::state::age(p["timestamp"].as_str().unwrap_or("")),
        p["summary"].as_str().unwrap_or("")
    );
}

fn print_status(s: &serde_json::Value) {
    println!(
        "{}/{}  ({} messages)  executor: {}  policy: {}",
        s["project"].as_str().unwrap_or(""),
        s["room"].as_str().unwrap_or(""),
        s["messages"],
        s["executor"].as_str().unwrap_or("-"),
        s["policy"].as_str().unwrap_or("")
    );
    let props: Vec<&serde_json::Value> = s["proposals"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let groups = [
        ("pending", "awaiting votes"),
        ("approved", "approved, not completed"),
        ("rejected", "rejected, not revised"),
        ("abstained", "abstained, still open"),
    ];
    let mut any_open = false;
    for (state, title) in groups {
        let in_group: Vec<&&serde_json::Value> = props.iter().filter(|p| p["state"] == state).collect();
        if in_group.is_empty() {
            continue;
        }
        any_open = true;
        println!("  {title}:");
        for p in in_group {
            print_proposal_line(p);
        }
    }
    if !any_open && !props.is_empty() {
        println!("  no open proposals");
    }
    let done: Vec<&&serde_json::Value> = props.iter().filter(|p| p["state"] == "completed" || p["state"] == "superseded").collect();
    if !done.is_empty() {
        let recent: Vec<String> = done
            .iter()
            .rev()
            .take(3)
            .map(|p| {
                let by = p["completed_by"].as_u64().or(p["superseded_by"].as_u64()).unwrap_or(0);
                format!("#{} {} by #{by}", p["id"], p["state"].as_str().unwrap_or(""))
            })
            .collect();
        println!("  recent: {}", recent.join(", "));
    }
    if let Some(n) = s["informational_votes"].as_u64() {
        if n > 0 {
            println!("  informational votes (unlinked, self, or non-participant): {n}");
        }
    }
    if let Some(lp) = s["last_post"].as_object() {
        if lp.is_empty() {
            println!("  no posts yet");
        }
        for (a, t) in lp {
            println!("  last post by {:<8} {}", a, t.as_str().unwrap_or(""));
        }
    }
    if let Some(open) = s["open_handoffs"].as_array() {
        for h in open {
            let text = h["handoff"].as_str().unwrap_or("").replace('\n', " ");
            let text: String = text.chars().take(110).collect();
            println!("  open handoff from {}: {}", h["agent"].as_str().unwrap_or(""), text);
        }
    }
}

pub fn status(room: Option<String>, all: bool, json: bool) -> Result<()> {
    let mut targets: Vec<(Project, Vec<String>)> = Vec::new();
    if all {
        for (name, root) in registry::live_projects() {
            if paths::is_initialized(&root) {
                let names = room_names(&root)?;
                targets.push((Project { root, name }, names));
            }
        }
    } else if let Some(r) = room {
        let rr = paths::resolve_room(Some(&r))?;
        paths::ensure_room_exists(&rr)?;
        targets.push((rr.project.clone(), vec![rr.room.clone()]));
    } else {
        let project = paths::current_project()?;
        paths::ensure_initialized(&project)?;
        let names = room_names(&project.root)?;
        targets.push((project, names));
    }
    let mut out = Vec::new();
    for (project, names) in &targets {
        for n in names {
            out.push(room_status(project, n)?);
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if out.is_empty() {
        println!("no rooms found");
    } else {
        for s in &out {
            print_status(s);
        }
    }
    Ok(())
}

pub fn home() -> Result<()> {
    if crate::menu::interactive() {
        return crate::menu::run();
    }
    match paths::current_project() {
        Ok(p) if paths::is_initialized(&p.root) => {
            status(None, false, false)?;
            let cfg = config::load(&p.root);
            let parts = agent::participants(&cfg);
            println!();
            println!("agents: {}", parts.join(", "));
            println!("  kickoff prompt:  cowork prompt <agent> [--copy]");
            println!("  rules block:     cowork prompt <agent> --rules");
            println!("  check setup:     cowork doctor");
            println!("  help:            cowork --help");
        }
        _ => {
            println!("cowork is not set up here.");
            println!("Run `cowork init` inside a git repository to create .ai-common/, write the rules blocks, and print the prompts for each agent.");
        }
    }
    Ok(())
}

// ---------- prompt ----------

fn copy_to_clipboard(text: &str) -> Result<String> {
    let candidates: [(&str, &[&str]); 3] = [
        ("clip.exe", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("wl-copy", &[]),
    ];
    for (bin, args) in candidates {
        let child = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            if let Some(mut stdin) = c.stdin.take() {
                stdin.write_all(text.as_bytes())?;
            }
            if c.wait()?.success() {
                return Ok(bin.to_string());
            }
        }
    }
    bail!("no clipboard tool found (tried clip.exe, xclip, wl-copy)")
}

pub fn prompt(agent_name: &str, rules: bool, copy: bool, room_name: Option<&str>) -> Result<()> {
    let project = paths::current_project()?;
    let cfg = config::load(&project.root);
    let parts = agent::participants(&cfg);
    let other = agent::counterpart(agent_name, &parts);
    let text = if let Some(r) = room_name {
        room_prompts(&project, r)?
            .into_iter()
            .find(|(a, _, _)| a == agent_name)
            .map(|(_, _, t)| t)
            .ok_or_else(|| anyhow!("`{agent_name}` is not a participant of room `{r}`"))?
    } else if rules {
        templates::rules(&project.root, agent_name, &other, &project.name)
    } else {
        templates::kickoff(&project.root, agent_name, &other, &project.name)
    };
    if copy {
        let tool = copy_to_clipboard(&text)?;
        eprintln!("(copied to clipboard via {tool})");
    }
    println!("{}", text.trim_end());
    Ok(())
}

// ---------- lock ----------

pub fn lock(path: &str, command: &[String]) -> Result<i32> {
    if command.is_empty() {
        bail!("usage: cowork lock <path> -- <command...>");
    }
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)
        .with_context(|| format!("opening {path}"))?;
    f.lock_exclusive()?;
    let status = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .with_context(|| format!("running {}", command[0]))?;
    let _ = f.unlock();
    Ok(status.code().unwrap_or(1))
}

// ---------- archive ----------

pub fn archive(room_addr: Option<String>, keep: Option<usize>) -> Result<()> {
    let rr: RoomRef = paths::resolve_room(room_addr.as_deref())?;
    paths::ensure_room_exists(&rr)?;
    let cfg = config::load(&rr.project.root);
    let keep = keep.or(cfg.archive_keep).unwrap_or(20);
    let adir = paths::archive_dir(&rr.project.root);
    fs::create_dir_all(&adir)?;
    let date = Utc::now().format("%Y%m%d").to_string();
    let fname = format!("{}-{}.md", rr.room, date);
    let apath = adir.join(&fname);
    let mut moved = 0usize;
    let addr = rr.addr();
    let cfg_participants = agent::participants(&cfg);
    let mut kept_live = 0usize;
    room::rewrite_locked(&rr.path, |content| {
        let rf = room::parse(content);
        if rf.messages.len() <= keep {
            return Ok(None);
        }
        // The newest `keep` messages always stay. Older messages that belong to an
        // unresolved proposal thread stay too, so proposal state survives archiving.
        let participants = rf.participants(&cfg_participants);
        let live = crate::state::live_thread_ids(&rf, &participants);
        let tail_start = rf.messages.len() - keep;
        let mut old: Vec<Message> = Vec::new();
        let mut new: Vec<Message> = Vec::new();
        for (i, m) in rf.messages.iter().enumerate() {
            let protected = m.id.map(|x| live.contains(&x)).unwrap_or(false);
            if i >= tail_start {
                new.push(m.clone());
            } else if protected {
                kept_live += 1;
                new.push(m.clone());
            } else {
                old.push(m.clone());
            }
        }
        if old.is_empty() {
            return Ok(None);
        }
        let n = old.len();
        let (old, new) = (old.as_slice(), new.as_slice());
        let mut atext = String::new();
        if !apath.exists() {
            atext.push_str(&format!("# Archive of {addr} ({date})\n\n"));
        }
        for m in old {
            atext.push_str(&m.render());
        }
        let mut af = OpenOptions::new().append(true).create(true).open(&apath)?;
        af.write_all(atext.as_bytes())?;
        af.sync_all()?;
        moved = n;
        // Ids stay contiguous: record the high-water mark before dropping messages.
        let mut front = rf.front.clone();
        if let Some(f) = front.as_mut() {
            f.last_id = rf.last_id();
        }
        let mut out = front.map(|f| f.render()).unwrap_or_default();
        out.push_str(&rf.head);
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(&format!(
            "> Archived {n} messages to archive/{fname} on {}\n\n",
            room::now_ts()
        ));
        for m in new {
            out.push_str(&m.render());
        }
        Ok(Some(out))
    })?;
    if moved == 0 {
        println!("nothing to archive: {addr} has {keep} or fewer messages that are not part of an open proposal");
    } else {
        println!("archived {moved} messages from {addr} to {}", apath.display());
    }
    if kept_live > 0 {
        println!("kept {kept_live} older message(s) that belong to open proposals");
    }
    Ok(())
}

// ---------- config ----------

pub fn config_cmd(action: ConfigCmd) -> Result<()> {
    let project = paths::current_project()?;
    match action {
        ConfigCmd::Set { key, value } => {
            config::set(&project.root, &key, &value)?;
            println!("{key} = {value}  ({})", paths::config_path(&project.root).display());
        }
        ConfigCmd::Get { key } => match config::get(&project.root, &key)? {
            Some(v) => println!("{v}"),
            None => println!("(unset)"),
        },
        ConfigCmd::List => {
            let cfg = config::load(&project.root);
            println!("name         = {}", cfg.name.clone().unwrap_or_else(|| format!("{} (default: directory name)", project.name)));
            println!("agents       = {}", agent::participants(&cfg).join(","));
            println!("tail         = {}", cfg.tail.unwrap_or(40));
            println!("wait_timeout = {}", cfg.wait_timeout.unwrap_or(300));
            println!("archive_keep = {}", cfg.archive_keep.unwrap_or(20));
            let p = paths::config_path(&project.root);
            println!("({} {})", p.display(), if p.exists() { "exists" } else { "not created; defaults in use" });
        }
    }
    Ok(())
}

// ---------- daemon ----------

pub fn daemon_cmd(action: DaemonCmd) -> Result<()> {
    match action {
        DaemonCmd::Run => daemon::run_server(),
        DaemonCmd::Start => {
            daemon::ensure_running()?;
            match daemon::ping() {
                Some(v) => println!("daemon running (pid {}, socket {})", v["pid"], paths::socket_path().display()),
                None => bail!("daemon started but does not answer"),
            }
            Ok(())
        }
        DaemonCmd::Stop => {
            if daemon::stop()? {
                println!("daemon stopped");
            } else {
                println!("daemon was not running");
            }
            Ok(())
        }
        DaemonCmd::Status => {
            match daemon::ping() {
                Some(v) => println!(
                    "running: pid {}, up {}s, watching {} dirs, socket {}",
                    v["pid"], v["uptime_s"], v["watched_dirs"], paths::socket_path().display()
                ),
                None => println!("not running (auto-starts when `cowork wait` needs it)"),
            }
            Ok(())
        }
    }
}

// ---------- doctor ----------

pub fn doctor() -> Result<()> {
    let mut problems = 0;
    let mut line = |ok: bool, warn_only: bool, msg: String| {
        let tag = if ok { "ok  " } else if warn_only { "info" } else { "warn" };
        if !ok && !warn_only {
            problems += 1;
        }
        println!("[{tag}] {msg}");
    };

    // binary on PATH
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("cowork").is_file()))
        .unwrap_or(false);
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    line(on_path, false, if on_path { format!("`cowork` is on PATH ({exe})") } else { format!("`cowork` is not on PATH (running {exe}); add its directory, e.g. ~/.local/bin, to PATH") });

    // project
    let project = paths::current_project().ok();
    match &project {
        Some(p) => {
            let init = paths::is_initialized(&p.root);
            line(init, false, if init { format!("project `{}` initialized at {}", p.name, p.root.display()) } else { format!("project `{}` found at {} but not initialized; run `cowork init`", p.name, p.root.display()) });
            let git = p.root.join(".git").exists();
            line(git, true, if git { "git repository detected".into() } else { "no .git at project root (initialized with --no-git?)".into() });
            if init {
                let main = paths::room_path(&p.root, "main").exists();
                line(main, false, if main { "room main exists".into() } else { "room main is missing; run `cowork init`".into() });
                let cfg = config::load(&p.root);
                for a in agent::participants(&cfg) {
                    let f = p.root.join(templates::rules_file(&a));
                    let has = fs::read_to_string(&f).map(|s| s.contains(templates::START) || s.contains(templates::LEGACY_START)).unwrap_or(false);
                    line(has, false, if has { format!("rules block for {a} present in {}", templates::rules_file(&a)) } else { format!("rules block for {a} missing from {}; run `cowork init`", templates::rules_file(&a)) });
                }
                for t in ["claude", "codex"] {
                    if agent::participants(&cfg).iter().any(|a| a == t) {
                        let has = crate::hooks::installed(&p.root, t);
                        let f = crate::hooks::hook_file(&p.root, t);
                        let f = f.strip_prefix(&p.root).map(|x| x.display().to_string()).unwrap_or_default();
                        line(has, false, if has { format!("room hooks for {t} installed in {f}") } else { format!("room hooks for {t} missing from {f}; run `cowork hook install`") });
                    }
                }
                let reg = registry::lookup(&p.name).is_some();
                line(reg, false, if reg { "project registered for cross-project addressing".into() } else { "project not in registry; run `cowork init` again".into() });
            }
        }
        None => line(false, false, "not inside a project: run `cowork init` inside a git repository".into()),
    }

    // agent identity
    match agent::detect_env() {
        Some((a, src)) => line(true, false, format!("calling agent detected as `{a}` (via {src})")),
        None => line(false, true, "calling agent unknown in this shell; agents' own shells set markers, otherwise use COWORK_AGENT=<name> or --agent".into()),
    }

    // daemon
    match daemon::ping() {
        Some(v) => line(true, false, format!("daemon running (pid {}, watching {} dirs)", v["pid"], v["watched_dirs"])),
        None => line(false, true, "daemon not running; it auto-starts when `cowork wait` needs it (`cowork daemon start` to test)".into()),
    }

    // feedback endpoint
    let (ok, msg) = crate::feedback::describe();
    line(ok, true, msg);

    // clipboard
    let clip = ["clip.exe", "xclip", "wl-copy"].iter().find(|b| which(b));
    match clip {
        Some(b) => line(true, false, format!("clipboard tool available: {b}")),
        None => line(false, true, "no clipboard tool (clip.exe/xclip/wl-copy); `cowork prompt --copy` will not work".into()),
    }

    if problems == 0 {
        println!("all checks passed");
    } else {
        println!("{problems} problem(s) found");
    }
    Ok(())
}

pub fn clipboard_available() -> bool {
    ["clip.exe", "xclip", "wl-copy"].iter().any(|b| which(b))
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}
