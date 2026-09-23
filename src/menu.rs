//! Interactive prompts for bare `cowork` on a terminal. This is the human surface:
//! arrow keys to move, Enter to select, Esc to go back. Agents use the flags.

use crate::cli::{DaemonCmd, PostArgs, ReadArgs};
use crate::paths::Project;
use crate::{agent, commands, config, paths, templates};
use anyhow::Result;
use inquire::list_option::ListOption;
use inquire::validator::Validation;
use inquire::{Confirm, InquireError, MultiSelect, Select, Text};
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

pub fn agent_files(default_yes: bool) -> Result<Option<bool>> {
    cancellable(
        Confirm::new("Create or update AGENTS.md and CLAUDE.md?")
            .with_default(default_yes)
            .with_help_message("yes: write cowork rules for configured agents · no: leave files untouched · esc: cancel")
            .prompt(),
    )
}

/// The prompts the agent-selection flows ask through. `Term` is the terminal;
/// tests script it. The flows only ask and return a plan, so leaving any prompt
/// with Esc writes nothing.
trait Ui {
    fn select(&mut self, title: &str, options: Vec<Opt>) -> Result<Option<String>>;
    fn text(&mut self, prompt: &str, default: Option<&str>, help: &str) -> Result<Option<String>>;
    fn multi(&mut self, title: &str, offered: Vec<String>, preselected: &[usize]) -> Result<Option<Vec<String>>>;
}

struct Term;

impl Ui for Term {
    fn select(&mut self, title: &str, options: Vec<Opt>) -> Result<Option<String>> {
        select(title, options)
    }

    fn text(&mut self, prompt: &str, default: Option<&str>, help: &str) -> Result<Option<String>> {
        text(prompt, default, help)
    }

    fn multi(&mut self, title: &str, offered: Vec<String>, preselected: &[usize]) -> Result<Option<Vec<String>>> {
        let valid = |picked: &[ListOption<&String>]| {
            let list: Vec<String> = picked.iter().map(|o| o.value.clone()).collect();
            Ok(match agent::validate_roster(&list) {
                Ok(()) => Validation::Valid,
                Err(e) => Validation::Invalid(e.to_string().into()),
            })
        };
        let page = offered.len().clamp(4, 14);
        let r = MultiSelect::new(title, offered)
            .with_default(preselected)
            .with_validator(valid)
            .with_help_message("↑↓ move · space toggle · enter confirm · esc back")
            .with_page_size(page)
            .with_vim_mode(false)
            .prompt();
        cancellable(r)
    }
}

/// What an agent picker offers: the known tools, then any other name in
/// `current`, with `current` preselected.
fn agent_choices(current: &[String]) -> (Vec<String>, Vec<usize>) {
    let mut offered: Vec<String> = agent::KNOWN_AGENTS.iter().map(|a| a.to_string()).collect();
    for a in current {
        if !offered.contains(a) {
            offered.push(a.clone());
        }
    }
    let pre = offered.iter().enumerate().filter(|(_, a)| current.contains(a)).map(|(i, _)| i).collect();
    (offered, pre)
}

/// Ask which agents take part. Names already in `current` keep their order, so
/// the default executor (the first one) does not change behind the user's back.
fn choose_agents(ui: &mut dyn Ui, title: &str, current: &[String]) -> Result<Option<Vec<String>>> {
    let (offered, pre) = agent_choices(current);
    let Some(picked) = ui.multi(title, offered, &pre)? else { return Ok(None) };
    let mut out: Vec<String> = current.iter().filter(|a| picked.contains(a)).cloned().collect();
    out.extend(picked.into_iter().filter(|a| !current.contains(a)));
    Ok(Some(out))
}

#[derive(Debug, PartialEq)]
struct RoomPlan {
    name: String,
    purpose: Option<String>,
    agents: Vec<String>,
    executor: String,
}

fn plan_room(ui: &mut dyn Ui, pool: &[String]) -> Result<Option<RoomPlan>> {
    let Some(name) = ui.text("Room name", None, "letters, digits, - _ . · esc back")? else { return Ok(None) };
    if name.is_empty() {
        return Ok(None);
    }
    let Some(purpose) = ui.text("What is this room for?", None, "optional, enter to skip")? else { return Ok(None) };
    let Some(agents) = choose_agents(ui, "Which AIs take part in this room?", pool)? else { return Ok(None) };
    let options = agents.iter().map(|a| opt(a.clone(), a.clone())).collect();
    let Some(executor) = ui.select("Which agent executes (writes the changes)? The others advise and vote.", options)? else {
        return Ok(None);
    };
    Ok(Some(RoomPlan { name, purpose: Some(purpose).filter(|s| !s.is_empty()), agents, executor }))
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
        "init" | "init-nogit" => {
            let root = git.clone().unwrap_or_else(|| cwd.clone());
            let current = agent::participants(&config::load(&root));
            let Some(agents) = choose_agents(&mut Term, "Which AIs will work here?", &current)? else { return Ok(true) };
            report(commands::init(Some(agents.join(",")), key == "init-nogit", false, None));
        }
        "explain" => {
            println!();
            println!("Setup from this menu will:");
            println!("  1. ask which AIs work here (claude, codex, kimi; 2 or 3), then create .ai-common/rooms/main.md,");
            println!("     the shared log, plus PROTOCOL.md (kept as is if it exists)");
            println!("  2. ask whether to add cowork rules to CLAUDE.md and AGENTS.md (existing text is preserved)");
            println!("  3. install editor hooks for the selected claude and codex agents (.claude/settings.json,");
            println!("     .codex/hooks.json); Kimi Code's hooks are global: `cowork hook install --tool kimi`");
            println!("  4. add .ai-common/.cursors/ to .gitignore (per-agent read positions)");
            println!("  5. register the project for <project>/<room> addressing");
            println!("  6. print one kickoff prompt per agent to paste into its session");
            println!("It is idempotent: running it again refreshes hooks and prompts, and optionally rules blocks.");
        }
        "doctor" => report(commands::doctor()),
        "list-all" => report(commands::list(true)),
        _ => return Ok(false),
    }
    Ok(true)
}

// ---------- initialized ----------

/// Who is acting. Humans come first because the menu is the human surface.
fn who(members: &[String]) -> Result<Option<String>> {
    let mut options = vec![opt("human", "you (posts as `human`)")];
    for a in members {
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
    let Some(plan) = plan_room(&mut Term, parts)? else { return Ok(()) };
    println!();
    report(commands::new_room(&plan.name, plan.purpose, Some(plan.executor), Some(plan.agents), None));
    if paths::room_path(&p.root, &plan.name).exists() {
        copy_prompt_offer(p, &plan.name)?;
    }
    Ok(())
}

/// Change which AIs new rooms get by default. Existing rooms, main included, keep
/// their members: votes are judged against a room's roster, so changing it would
/// change what past votes meant.
fn choose_default_agents(p: &Project, current: &[String]) -> Result<()> {
    let Some(picked) = choose_agents(&mut Term, "Which AIs take part in new rooms?", current)? else { return Ok(()) };
    println!();
    if picked == current {
        println!("unchanged: new rooms take {}", picked.join(", "));
        return Ok(());
    }
    config::set(&p.root, "agents", &picked.join(","))?;
    println!("new rooms now take {} by default", picked.join(", "));
    let added: Vec<String> = picked.iter().filter(|a| !current.contains(a)).cloned().collect();
    let hooked: Vec<String> = added.iter().filter(|a| *a == "claude" || *a == "codex").cloned().collect();
    for t in &hooked {
        let path = crate::hooks::install_for(&p.root, t, t)?;
        println!("installed room hooks for {t} in {}", path.strip_prefix(&p.root).unwrap_or(&path).display());
    }
    crate::hooks::print_trust_notes(&hooked);
    for h in crate::hooks::setup_hints(&p.root, &added) {
        println!("  {h}");
    }
    let main = commands::room_members(&p.root, "main")?;
    println!("existing rooms keep their members; main has {}", main.join(", "));
    for a in added.iter().filter(|a| !main.contains(a)) {
        println!("  {a} is not in main: create a room with it, then paste that room's prompt into its session");
    }
    Ok(())
}

fn room_menu(p: &Project, room: &str) -> Result<()> {
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
                let Some(me) = who(&commands::room_members(&p.root, room)?)? else { continue };
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
                let members = commands::room_members(&p.root, room)?;
                let Some(me) = pick_agent("Read as which agent?", &members)? else { continue };
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

fn open_room(p: &Project) -> Result<()> {
    let names = commands::room_names(&p.root)?;
    let mut options: Vec<Opt> = names.iter().map(|n| opt(n.clone(), n.clone())).collect();
    options.push(opt("all", "Show every project on this machine"));
    let Some(pick) = select("Which room?", options)? else { return Ok(()) };
    if pick == "all" {
        println!();
        report(commands::list(true));
        return Ok(());
    }
    room_menu(p, &pick)
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
        opt("agents", format!("Choose AIs for new rooms (now: {})", parts.join(", "))),
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
        "open" => open_room(p)?,
        "agents" => choose_default_agents(p, &parts)?,
        "kickoff" => {
            // The setup prompt places an agent in main, so only main's members get one.
            let members = commands::room_members(&p.root, "main")?;
            for a in parts.iter().filter(|a| !members.contains(a)) {
                println!("{a} is not in main: open a room it belongs to and copy that room's prompt instead");
            }
            let can_copy = commands::clipboard_available();
            loop {
                let mut options: Vec<Opt> = members
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
                println!("── setup prompt for {a} (rules: {}) ──", templates::rules_source(&p.root, &a));
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
        "init" => report(commands::init(None, false, false, None)),
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    enum A {
        Text(&'static str),
        Pick(&'static str),
        Multi(&'static [&'static str]),
        Esc,
    }

    /// Answers prompts from a list and records what each picker offered.
    struct Script {
        answers: VecDeque<A>,
        offered: Vec<(Vec<String>, Vec<usize>)>,
        choices: Vec<Vec<String>>,
    }

    fn script(answers: Vec<A>) -> Script {
        Script { answers: answers.into(), offered: Vec::new(), choices: Vec::new() }
    }

    impl Ui for Script {
        fn select(&mut self, _: &str, options: Vec<Opt>) -> Result<Option<String>> {
            self.choices.push(options.into_iter().map(|o| o.key).collect());
            match self.answers.pop_front() {
                Some(A::Pick(k)) => Ok(Some(k.to_string())),
                Some(A::Esc) => Ok(None),
                _ => panic!("unexpected select"),
            }
        }

        fn text(&mut self, _: &str, _: Option<&str>, _: &str) -> Result<Option<String>> {
            match self.answers.pop_front() {
                Some(A::Text(t)) => Ok(Some(t.to_string())),
                Some(A::Esc) => Ok(None),
                _ => panic!("unexpected text"),
            }
        }

        fn multi(&mut self, _: &str, offered: Vec<String>, preselected: &[usize]) -> Result<Option<Vec<String>>> {
            self.offered.push((offered, preselected.to_vec()));
            match self.answers.pop_front() {
                Some(A::Multi(v)) => Ok(Some(v.iter().map(|a| a.to_string()).collect())),
                Some(A::Esc) => Ok(None),
                _ => panic!("unexpected multi-select"),
            }
        }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn pickers_offer_the_known_agents_then_custom_names_with_current_preselected() {
        assert_eq!(agent_choices(&names(&["claude", "codex"])), (names(&["claude", "codex", "kimi"]), vec![0, 1]));
        assert_eq!(agent_choices(&names(&["gemini", "kimi"])), (names(&["claude", "codex", "kimi", "gemini"]), vec![2, 3]));
    }

    #[test]
    fn chosen_agents_keep_the_current_order_and_append_new_ones() {
        let mut ui = script(vec![A::Multi(&["claude", "codex", "kimi"])]);
        let got = choose_agents(&mut ui, "t", &names(&["codex", "claude"])).unwrap();
        assert_eq!(got, Some(names(&["codex", "claude", "kimi"])));
        let mut ui = script(vec![A::Esc]);
        assert_eq!(choose_agents(&mut ui, "t", &names(&["claude", "codex"])).unwrap(), None);
    }

    #[test]
    fn esc_at_any_room_creation_prompt_plans_nothing() {
        let full = || vec![A::Text("r1"), A::Text("why"), A::Multi(&["claude", "codex", "kimi"]), A::Pick("kimi")];
        for stop in 0..4 {
            let mut answers: Vec<A> = full().into_iter().take(stop).collect();
            answers.push(A::Esc);
            let mut ui = script(answers);
            assert_eq!(plan_room(&mut ui, &names(&["claude", "codex"])).unwrap(), None, "esc at prompt {stop}");
            assert!(ui.answers.is_empty(), "no prompt after esc at {stop}");
        }
        // An empty name also backs out before anything else is asked.
        let mut ui = script(vec![A::Text("")]);
        assert_eq!(plan_room(&mut ui, &names(&["claude", "codex"])).unwrap(), None);
    }

    #[test]
    fn a_room_plan_offers_only_the_selected_agents_as_executor() {
        let mut ui = script(vec![A::Text("r1"), A::Text(""), A::Multi(&["claude", "kimi"]), A::Pick("kimi")]);
        let plan = plan_room(&mut ui, &names(&["claude", "codex"])).unwrap().unwrap();
        assert_eq!(
            plan,
            RoomPlan { name: "r1".into(), purpose: None, agents: names(&["claude", "kimi"]), executor: "kimi".into() }
        );
        assert_eq!(ui.offered, vec![(names(&["claude", "codex", "kimi"]), vec![0, 1])]);
        assert_eq!(ui.choices, vec![names(&["claude", "kimi"])]);
    }
}
