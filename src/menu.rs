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
        let valid = |picked: &[ListOption<&Opt>]| {
            let list: Vec<String> = picked.iter().map(|o| o.value.key.clone()).collect();
            Ok(match agent::validate_roster(&list) {
                Ok(()) => Validation::Valid,
                Err(e) => Validation::Invalid(e.to_string().into()),
            })
        };
        let page = offered.len().clamp(4, 14);
        let options: Vec<Opt> = offered
            .into_iter()
            .map(|a| {
                if a == NEW_DSH {
                    return opt(a.clone(), format!("{a}  (one or more agents in DeepSeek Harness, any model; you name them next)"));
                }
                match agent::describe(&a) {
                    Some(what) => opt(a.clone(), format!("{a}  ({what})")),
                    None => opt(a.clone(), a),
                }
            })
            .collect();
        let r = MultiSelect::new(title, options)
            .with_default(preselected)
            .with_validator(valid)
            .with_help_message("↑↓ move · space toggle · enter confirm · esc back")
            .with_page_size(page)
            .with_vim_mode(false)
            .prompt();
        Ok(cancellable(r)?.map(|picked| picked.into_iter().map(|o| o.key).collect()))
    }
}

/// The picker entry that adds `dsh-<model>` agents; their models are asked next.
const NEW_DSH: &str = "dsh-<model>";

/// What an agent picker offers: the known tools, then any other name in
/// `current`, with `current` preselected, then the entry for another model in
/// DeepSeek Harness.
fn agent_choices(current: &[String]) -> (Vec<String>, Vec<usize>) {
    let mut offered: Vec<String> = agent::KNOWN_AGENTS.iter().map(|a| a.to_string()).collect();
    for a in current {
        if !offered.contains(a) {
            offered.push(a.clone());
        }
    }
    let pre = offered.iter().enumerate().filter(|(_, a)| current.contains(a)).map(|(i, _)| i).collect();
    offered.push(NEW_DSH.to_string());
    (offered, pre)
}

/// Ask which agents take part. Names already in `current` keep their order, so
/// the default executor (the first one) does not change behind the user's back.
fn choose_agents(ui: &mut dyn Ui, title: &str, current: &[String]) -> Result<Option<Vec<String>>> {
    let (offered, pre) = agent_choices(current);
    let Some(mut picked) = ui.multi(title, offered, &pre)? else { return Ok(None) };
    if let Some(i) = picked.iter().position(|a| a == NEW_DSH) {
        let others: Vec<String> = picked.iter().filter(|a| *a != NEW_DSH).cloned().collect();
        let Some(names) = name_dsh_agents(ui, &others)? else { return Ok(None) };
        picked.splice(i..=i, names);
    }
    let mut out: Vec<String> = current.iter().filter(|a| picked.contains(a)).cloned().collect();
    out.extend(picked.into_iter().filter(|a| !current.contains(a)));
    Ok(Some(out))
}

/// Ask which models the new DeepSeek Harness agents run, one agent per entry: the
/// harness is free and runs any model, so a room may hold several, even of one model.
/// Each is `dsh-<model>`; a name already taken gets the next number (`dsh-qwen-2`).
/// An empty answer backs out, like an empty room name.
fn name_dsh_agents(ui: &mut dyn Ui, picked: &[String]) -> Result<Option<Vec<String>>> {
    'ask: loop {
        let Some(answer) = ui.text(
            "Which models will DeepSeek Harness run? One agent per entry, comma-separated",
            None,
            "e.g. qwen or qwen, qwen, glm-4; each agent is named dsh-<model>, repeats are numbered · esc back",
        )?
        else {
            return Ok(None);
        };
        let mut names: Vec<String> = Vec::new();
        for model in answer.split(',').map(|m| m.trim().to_ascii_lowercase()).filter(|m| !m.is_empty()) {
            let model = model.strip_prefix(agent::DSH_PREFIX).unwrap_or(&model).to_string();
            if model.is_empty() || !model.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
                println!("use only letters, digits, - _ . in model names, separated by commas");
                continue 'ask;
            }
            let base = format!("{}{model}", agent::DSH_PREFIX);
            let taken = |n: &String| picked.contains(n) || names.contains(n);
            let name = std::iter::once(base.clone())
                .chain((2..).map(|i| format!("{base}-{i}")))
                .find(|n| !taken(n))
                .expect("an unused number exists");
            names.push(name);
        }
        if names.is_empty() {
            return Ok(None);
        }
        return Ok(Some(names));
    }
}

#[derive(Debug, PartialEq)]
struct RoomPlan {
    name: String,
    purpose: Option<String>,
    agents: Vec<String>,
    executor: String,
    advisor_tests: bool,
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
    let advisors: Vec<&str> = agents.iter().map(String::as_str).filter(|a| *a != executor).collect();
    let who = match advisors.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => advisors.concat(),
    };
    let title = format!("Let {who} also create and run test scripts and run commands? Only {executor} changes source code.");
    let options = vec![
        opt("review", "No: review and vote only"),
        opt("tests", "Yes: review, vote, and test (no source code changes)"),
    ];
    let Some(mode) = ui.select(&title, options)? else { return Ok(None) };
    Ok(Some(RoomPlan {
        name,
        purpose: Some(purpose).filter(|s| !s.is_empty()),
        agents,
        executor,
        advisor_tests: mode == "tests",
    }))
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
            println!("  1. ask which AIs work here (two or more of claude, codex, kimi, deepseek, argon, muse, or");
            println!("     dsh-<model> agents for any models in DeepSeek Harness), then create .ai-common/rooms/main.md,");
            println!("     the shared log, plus PROTOCOL.md (kept as is if it exists)");
            println!("  2. ask whether to add cowork rules to CLAUDE.md and AGENTS.md (existing text is preserved)");
            println!("  3. install editor hooks for the selected claude, codex, and muse agents (.claude/settings.json,");
            println!("     .codex/hooks.json, .muse/hooks.json); Kimi Code's hooks are global: `cowork hook install");
            println!("     --tool kimi`; DeepSeek Harness and Antigravity get no hooks and wait with `cowork wait`");
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
    report(commands::new_room(&plan.name, plan.purpose, Some(plan.executor), Some(plan.agents), plan.advisor_tests, None));
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
    let hooked: Vec<String> = added.iter().filter(|a| crate::hooks::is_project_tool(a)).cloned().collect();
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

    /// Answers prompts from a list and records what each picker asked and offered.
    struct Script {
        answers: VecDeque<A>,
        offered: Vec<(Vec<String>, Vec<usize>)>,
        choices: Vec<Vec<String>>,
        titles: Vec<String>,
    }

    fn script(answers: Vec<A>) -> Script {
        Script { answers: answers.into(), offered: Vec::new(), choices: Vec::new(), titles: Vec::new() }
    }

    impl Ui for Script {
        fn select(&mut self, title: &str, options: Vec<Opt>) -> Result<Option<String>> {
            self.titles.push(title.to_string());
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
        const KNOWN: &[&str] = &["claude", "codex", "kimi", "deepseek", "argon", "muse", NEW_DSH];
        assert_eq!(agent_choices(&names(&["claude", "codex"])), (names(KNOWN), vec![0, 1]));
        assert_eq!(
            agent_choices(&names(&["gemini", "kimi"])),
            (names(&["claude", "codex", "kimi", "deepseek", "argon", "muse", "gemini", NEW_DSH]), vec![2, 6])
        );
        assert_eq!(agent_choices(&names(&["muse", "argon", "deepseek"])), (names(KNOWN), vec![3, 4, 5]));
        // A dsh-<model> agent already on the list is offered by name, ahead of the entry for a new one.
        let (offered, pre) = agent_choices(&names(&["claude", "dsh-qwen"]));
        assert_eq!(&offered[6..], &names(&["dsh-qwen", NEW_DSH])[..]);
        assert_eq!(pre, vec![0, 6]);
    }

    #[test]
    fn every_known_agent_and_dsh_model_shows_its_tool_in_the_picker() {
        for a in agent::KNOWN_AGENTS {
            assert!(agent::tool_name(a).is_some() && agent::describe(a).is_some(), "{a} has no tool name");
        }
        assert_eq!(agent::describe("argon").as_deref(), Some("Gemini 4 Argon in Google Antigravity CLI"));
        assert_eq!(agent::describe("dsh-qwen").as_deref(), Some("qwen in DeepSeek Harness"));
        assert_eq!(agent::tool_name("dsh-qwen"), Some("DeepSeek Harness"));
        assert_eq!(agent::known_tool("DSH-GLM"), Some("deepseek"));
        for other in ["gemini", "dsh-", "dshqwen"] {
            assert_eq!((agent::known_tool(other), agent::describe(other)), (None, None), "{other}");
        }
    }

    #[test]
    fn another_model_in_deepseek_harness_is_named_dsh_model() {
        let mut ui = script(vec![A::Multi(&["claude", NEW_DSH]), A::Text("Qwen")]);
        assert_eq!(choose_agents(&mut ui, "t", &names(&["claude", "codex"])).unwrap(), Some(names(&["claude", "dsh-qwen"])));
        // A bad name is asked again; a full name is accepted, and a taken one gets the next number.
        let mut ui = script(vec![A::Multi(&["dsh-glm", NEW_DSH]), A::Text("qwen 3"), A::Text("dsh-glm")]);
        assert_eq!(choose_agents(&mut ui, "t", &names(&["dsh-glm", "claude"])).unwrap(), Some(names(&["dsh-glm", "dsh-glm-2"])));
        assert!(ui.answers.is_empty());
        // Esc or an empty name at the model question backs out of the whole choice.
        for answer in [A::Esc, A::Text(""), A::Text(" , ")] {
            let mut ui = script(vec![A::Multi(&["claude", NEW_DSH]), answer]);
            assert_eq!(choose_agents(&mut ui, "t", &names(&["claude", "codex"])).unwrap(), None);
        }
    }

    #[test]
    fn several_agents_can_share_deepseek_harness_even_on_one_model() {
        // One entry per agent: repeats are numbered, past the names already in the room too.
        let mut ui = script(vec![A::Multi(&["claude", "deepseek", "dsh-qwen", NEW_DSH]), A::Text("qwen, qwen, GLM-4, deepseek")]);
        let got = choose_agents(&mut ui, "t", &names(&["claude", "dsh-qwen"])).unwrap().unwrap();
        assert_eq!(got, names(&["claude", "dsh-qwen", "deepseek", "dsh-qwen-2", "dsh-qwen-3", "dsh-glm-4", "dsh-deepseek"]));
        for a in &got[1..] {
            assert_eq!(agent::known_tool(a), Some("deepseek"), "{a} runs in DeepSeek Harness");
        }
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
        let full = || vec![A::Text("r1"), A::Text("why"), A::Multi(&["claude", "codex", "kimi"]), A::Pick("kimi"), A::Pick("tests")];
        for stop in 0..5 {
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
        let mut ui = script(vec![A::Text("r1"), A::Text(""), A::Multi(&["claude", "kimi"]), A::Pick("kimi"), A::Pick("review")]);
        let plan = plan_room(&mut ui, &names(&["claude", "codex"])).unwrap().unwrap();
        assert_eq!(
            plan,
            RoomPlan {
                name: "r1".into(),
                purpose: None,
                agents: names(&["claude", "kimi"]),
                executor: "kimi".into(),
                advisor_tests: false,
            }
        );
        let mut offered = names(agent::KNOWN_AGENTS);
        offered.push(NEW_DSH.to_string());
        assert_eq!(ui.offered, vec![(offered, vec![0, 1])]);
        assert_eq!(ui.choices, vec![names(&["claude", "kimi"]), names(&["review", "tests"])]);
    }

    #[test]
    fn the_step_after_the_executor_can_let_the_advisors_test() {
        let mut ui = script(vec![A::Text("r1"), A::Text(""), A::Multi(&["claude", "codex", "kimi"]), A::Pick("claude"), A::Pick("tests")]);
        let plan = plan_room(&mut ui, &names(&["claude", "codex"])).unwrap().unwrap();
        assert!(plan.advisor_tests && plan.executor == "claude", "{plan:?}");
        // The question names only the advisors, after the executor was chosen.
        let asked = ui.titles.last().unwrap();
        assert!(asked.contains("Let codex and kimi also create and run test scripts and run commands?"), "{asked}");
        assert!(asked.contains("Only claude changes source code"), "{asked}");
        assert_eq!(ui.choices.last().unwrap(), &names(&["review", "tests"]));
        // Any number of agents: the question lists every advisor.
        let pick: &[&str] = &["claude", "codex", "kimi", "deepseek", "muse"];
        let mut ui = script(vec![A::Text("r2"), A::Text(""), A::Multi(pick), A::Pick("kimi"), A::Pick("review")]);
        let plan = plan_room(&mut ui, &names(&["claude", "codex"])).unwrap().unwrap();
        assert_eq!((plan.agents.len(), plan.advisor_tests), (5, false));
        let asked = ui.titles.last().unwrap();
        assert!(asked.starts_with("Let claude, codex, deepseek and muse also create"), "{asked}");
    }
}
