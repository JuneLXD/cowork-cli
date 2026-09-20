//! Interactive prompts for bare `cowork` on a terminal. This is the human surface:
//! arrow keys to move, Enter to select, Esc to go back. Agents use the flags.

use crate::cli::{DaemonCmd, PostArgs, ReadArgs};
use crate::paths::Project;
use crate::{agent, commands, config, paths, templates};
use anyhow::Result;
use inquire::{Confirm, InquireError, Select, Text};
use std::fmt;
use std::io::IsTerminal;

const HELP: &str = "↑↓ move · enter select · esc back";

pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

struct Opt {
    key: String,
    label: String,
}

impl fmt::Display for Opt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

fn opt(key: impl Into<String>, label: impl Into<String>) -> Opt {
    Opt { key: key.into(), label: label.into() }
}

/// Map Esc / ctrl-c to `None` so every prompt can be backed out of.
fn cancellable<T>(r: Result<T, InquireError>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn select(title: &str, options: Vec<Opt>) -> Result<Option<String>> {
    let page = options.len().clamp(4, 14);
    let r = Select::new(title, options)
        .with_help_message(HELP)
        .with_page_size(page)
        .with_vim_mode(false)
        .prompt();
    Ok(cancellable(r)?.map(|o| o.key))
}

fn text(prompt: &str, default: Option<&str>, help: &str) -> Result<Option<String>> {
    let mut t = Text::new(prompt);
    if !help.is_empty() {
        t = t.with_help_message(help);
    }
    if let Some(d) = default {
        if !d.is_empty() {
            t = t.with_default(d);
        }
    }
    Ok(cancellable(t.prompt())?.map(|s| s.trim().to_string()))
}

fn multiline(prompt: &str) -> Result<Option<String>> {
    let mut lines: Vec<String> = Vec::new();
    loop {
        let label = if lines.is_empty() { prompt.to_string() } else { "…".to_string() };
        let help = if lines.is_empty() {
            "enter adds a line · empty line finishes · esc back"
        } else {
            "empty line finishes"
        };
        let Some(line) = text(&label, None, help)? else { return Ok(None) };
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    Ok(Some(lines.join("\n")))
}

pub fn confirm(prompt: &str, default_yes: bool) -> Result<bool> {
    Ok(cancellable(Confirm::new(prompt).with_default(default_yes).prompt())?.unwrap_or(false))
}

fn report(r: Result<()>) {
    if let Err(e) = r {
        println!("error: {e:#}");
    }
}

pub fn run() -> Result<()> {
    loop {
        let keep_going = match paths::current_project() {
            Ok(p) if paths::is_initialized(&p.root) => main_menu(&p)?,
            _ => setup_menu()?,
        };
        if !keep_going {
            break;
        }
    }
    Ok(())
}

// ---------- not initialized ----------

fn setup_menu() -> Result<bool> {
    let cwd = std::env::current_dir()?;
    let git = paths::find_git_root(&cwd);
    println!();
    println!("cowork is not set up here.");
    match &git {
        Some(r) => println!("git repository: {}", r.display()),
        None => println!("no git repository found above {}", cwd.display()),
    }
    let mut options = Vec::new();
    match &git {
        Some(r) => options.push(opt("init", format!("Set up this repository ({})", r.display()))),
        None => options.push(opt("init-nogit", format!("Set up this directory without git ({})", cwd.display()))),
    }
    options.push(opt("explain", "What does setup do?"));
    options.push(opt("list-all", "Show projects already set up on this machine"));
    options.push(opt("doctor", "Check the installation (doctor)"));
    options.push(opt("quit", "Quit"));

    let Some(key) = select("What would you like to do?", options)? else { return Ok(false) };
    match key.as_str() {
        "init" => report(commands::init(None, false, false)),
        "init-nogit" => report(commands::init(None, true, false)),
        "explain" => {
            println!();
            println!("`cowork init` will:");
            println!("  1. create .ai-common/rooms/main.md, the shared log, plus PROTOCOL.md (kept as is if it exists)");
            println!("  2. add a marked rules block to CLAUDE.md and AGENTS.md so each tool knows the protocol");
            println!("  3. install the editor hooks in .claude/settings.json and .codex/hooks.json");
            println!("  4. add .ai-common/.cursors/ to .gitignore (per-agent read positions)");
            println!("  5. register the project for <project>/<room> addressing");
            println!("  6. print one kickoff prompt per agent to paste into its session");
            println!("It is idempotent: running it again only refreshes the rules blocks, hooks, and prompts.");
        }
        "doctor" => report(commands::doctor()),
        "list-all" => report(commands::list(true)),
        _ => return Ok(false),
    }
    Ok(true)
}

// ---------- initialized ----------

/// Who is acting. Humans come first because the menu is the human surface.
fn who(cfg: &config::Config) -> Result<Option<String>> {
    let parts = agent::participants(cfg);
    let mut options = vec![opt("human", "you (posts as `human`)")];
    for a in &parts {
        options.push(opt(a.clone(), format!("as {a}")));
    }
    options.push(opt("other", "someone else…"));
    let Some(pick) = select("Post as whom?", options)? else { return Ok(None) };
    if pick == "other" {
        return Ok(text("Name", None, "esc back")?.filter(|s| !s.is_empty()));
    }
    Ok(Some(pick))
}

fn pick_agent(title: &str, parts: &[String]) -> Result<Option<String>> {
    let options = parts.iter().map(|a| opt(a.clone(), a.clone())).collect();
    select(title, options)
}

/// Stay on the prompt picker so both prompts can be copied one after the other.
/// Returns when the user picks the last option or presses Esc.
fn copy_prompt_offer(project: &Project, room: &str) -> Result<()> {
    let can_copy = commands::clipboard_available();
    loop {
        let prompts = commands::room_prompts(project, room)?;
        let mut options: Vec<Opt> = prompts
            .iter()
            .map(|(a, role, _)| {
                let verb = if can_copy { "Copy" } else { "Show" };
                opt(a.clone(), format!("{verb} the {role} prompt for {a}"))
            })
            .collect();
        options.push(opt("back", "Back"));
        let Some(pick) = select("Paste a prompt into each agent's session", options)? else { return Ok(()) };
        if pick == "back" {
            return Ok(());
        }
        println!();
        report(commands::prompt(&pick, false, can_copy, Some(room)));
        println!();
    }
}

fn create_room(p: &Project, parts: &[String]) -> Result<()> {
    let Some(name) = text("Room name", None, "letters, digits, - _ . · esc back")? else { return Ok(()) };
    if name.is_empty() {
        return Ok(());
    }
    let Some(purpose) = text("What is this room for?", None, "optional, enter to skip")? else { return Ok(()) };
    let Some(exec) = pick_agent("Which agent executes (writes the changes)? The others advise and vote.", parts)? else {
        return Ok(());
    };
    println!();
    report(commands::new_room(&name, Some(purpose).filter(|s| !s.is_empty()), Some(exec)));
    if paths::room_path(&p.root, &name).exists() {
        copy_prompt_offer(p, &name)?;
    }
    Ok(())
}

fn room_menu(p: &Project, cfg: &config::Config, room: &str) -> Result<()> {
    loop {
        println!();
        report(commands::status(Some(room.to_string()), false, false));
        let options = vec![
            opt("stream", "Stream live (new messages as they arrive, ctrl-c stops)"),
            opt("recent", "Show recent messages"),
            opt("prompts", "Show the role prompts for this room"),
            opt("post", "Post a note into the room"),
            opt("unread", "Read unread as an agent"),
            opt("delete", "Delete this room"),
            opt("back", "Back"),
        ];
        let Some(key) = select(&format!("{}/{room}", p.name), options)? else { return Ok(()) };
        match key.as_str() {
            "stream" => {
                println!();
                report(commands::stream(Some(room.to_string()), 5, false));
            }
            "recent" => {
                let Some(n) = text("How many messages", Some("5"), "")? else { continue };
                let n = n.parse::<usize>().unwrap_or(5);
                println!();
                report(commands::read(ReadArgs {
                    room: Some(room.to_string()),
                    unread: false,
                    tail: None,
                    last: Some(n),
                    since: None,
                    agent: None,
                    me: None,
                    json: false,
                    brief: false,
                    no_advance: true,
                }));
            }
            "prompts" => {
                println!();
                match commands::room_prompts_text(p, room) {
                    Ok(t) => print!("{t}"),
                    Err(e) => println!("error: {e:#}"),
                }
                copy_prompt_offer(p, room)?;
            }
            "post" => {
                let Some(me) = who(cfg)? else { continue };
                let Some(thoughts) = multiline("Message")? else { continue };
                if thoughts.trim().is_empty() {
                    println!("nothing posted: the message is empty");
                    continue;
                }
                let Some(handoff) = text("Question or request for the agents", None, "enter to skip")? else { continue };
                report(commands::post(PostArgs {
                    room: Some(room.to_string()),
                    agent: Some(me),
                    thoughts: Some(thoughts),
                    action: None,
                    taken: None,
                    handoff: Some(handoff),
                    vote: None,
                    re: None,
                    propose: false,
                    complete: false,
                    thoughts_file: None,
                    action_file: None,
                    taken_file: None,
                    handoff_file: None,
                }));
            }
            "unread" => {
                let parts = agent::participants(cfg);
                let Some(me) = pick_agent("Read as which agent?", &parts)? else { continue };
                println!();
                report(commands::read(ReadArgs {
                    room: Some(room.to_string()),
                    unread: true,
                    tail: None,
                    last: None,
                    since: None,
                    agent: None,
                    me: Some(me),
                    json: false,
                    brief: false,
                    no_advance: false,
                }));
            }
            "delete" => {
                if room == "main" {
                    println!("`main` is the default room and cannot be deleted. Use `cowork archive --room main` to clear it.");
                    continue;
                }
                report(commands::delete_room(room, false));
                if !paths::room_path(&p.root, room).exists() {
                    return Ok(());
                }
            }
            _ => return Ok(()),
        }
    }
}

fn open_room(p: &Project, cfg: &config::Config) -> Result<()> {
    let names = commands::room_names(&p.root)?;
    let mut options: Vec<Opt> = names.iter().map(|n| opt(n.clone(), n.clone())).collect();
    options.push(opt("all", "Show every project on this machine"));
    let Some(pick) = select("Which room?", options)? else { return Ok(()) };
    if pick == "all" {
        println!();
        report(commands::list(true));
        return Ok(());
    }
    room_menu(p, cfg, &pick)
}

fn main_menu(p: &Project) -> Result<bool> {
    let cfg = config::load(&p.root);
    let parts = agent::participants(&cfg);
    println!();
    report(commands::list(false));
    println!();
    let options = vec![
        opt("new", "Create a room"),
        opt("open", "Open a room (stream, recent messages, prompts, post)"),
        opt("kickoff", "Show the setup prompt for an agent"),
        opt("intro", "What is cowork?"),
        opt("doctor", "Check the setup (doctor)"),
        opt("daemon", "Daemon status"),
        opt("init", "Re-run setup (refresh rules blocks and prompts)"),
        opt("quit", "Quit"),
    ];
    let title = format!("{} · agents {} · what next?", p.name, parts.join(", "));
    let Some(key) = select(&title, options)? else { return Ok(false) };
    match key.as_str() {
        "new" => create_room(p, &parts)?,
        "open" => open_room(p, &cfg)?,
        "kickoff" => {
            let can_copy = commands::clipboard_available();
            loop {
                let mut options: Vec<Opt> = parts
                    .iter()
                    .map(|a| {
                        let verb = if can_copy { "Copy" } else { "Show" };
                        opt(a.clone(), format!("{verb} the setup prompt for {a}"))
                    })
                    .collect();
                options.push(opt("back", "Back"));
                let Some(a) = select("Setup prompts (paste into each agent's session)", options)? else { break };
                if a == "back" {
                    break;
                }
                println!();
                println!("── setup prompt for {a} (its rules are already in {}) ──", templates::rules_file(&a));
                report(commands::prompt(&a, false, can_copy, None));
                println!();
            }
        }
        "intro" => {
            println!();
            crate::intro::print();
        }
        "doctor" => report(commands::doctor()),
        "daemon" => report(commands::daemon_cmd(DaemonCmd::Status)),
        "init" => report(commands::init(None, false, false)),
        _ => return Ok(false),
    }
    Ok(true)
}
