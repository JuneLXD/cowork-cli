mod agent;
mod cli;
mod commands;
mod config;
mod cursor;
mod daemon;
mod feedback;
mod feedback_env;
mod hooks;
mod intro;
mod menu;
mod paths;
mod registry;
mod room;
mod state;
mod templates;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Cmd, HookCmd};

fn run(cli: Cli) -> Result<i32> {
    match cli.cmd {
        None => commands::home().map(|_| 0),
        Some(Cmd::Init { agents, no_git, quiet, agent_files }) => commands::init(agents, no_git, quiet, agent_files.choice()).map(|_| 0),
        Some(Cmd::Post(a)) => commands::post(a).map(|_| 0),
        Some(Cmd::Read(a)) => commands::read(a).map(|_| 0),
        Some(Cmd::Wait(a)) => commands::wait(a),
        Some(Cmd::New { name, purpose, executor, agents, agent_files }) => {
            commands::new_room(&name, purpose, executor, agents.as_deref().map(config::parse_agents), agent_files.choice()).map(|_| 0)
        }
        Some(Cmd::Delete { name, yes }) => commands::delete_room(&name, yes).map(|_| 0),
        Some(Cmd::Stream { room, last, json }) => commands::stream(room, last, json).map(|_| 0),
        Some(Cmd::List { all }) => commands::list(all).map(|_| 0),
        Some(Cmd::Status { room, all, json }) => commands::status(room, all, json).map(|_| 0),
        Some(Cmd::Prompt { agent, room, rules, copy }) => commands::prompt(&agent, rules, copy, room.as_deref()).map(|_| 0),
        Some(Cmd::Lock { path, command }) => commands::lock(&path, &command),
        Some(Cmd::Archive { room, keep }) => commands::archive(room, keep).map(|_| 0),
        Some(Cmd::Config { action }) => commands::config_cmd(action).map(|_| 0),
        Some(Cmd::Daemon { action }) => commands::daemon_cmd(action).map(|_| 0),
        Some(Cmd::Doctor) => commands::doctor().map(|_| 0),
        Some(Cmd::Hook { action }) => match action {
            HookCmd::Install { tool } => hooks::install(tool.as_deref()).map(|_| 0),
            HookCmd::Remove { tool } => hooks::remove(tool.as_deref()).map(|_| 0),
            HookCmd::Status => hooks::status().map(|_| 0),
            HookCmd::Stop { agent, timeout, format } => hooks::stop(agent.as_deref(), timeout, &format),
            HookCmd::Prompt { agent, format } => hooks::prompt(agent.as_deref(), &format).map(|_| 0),
            HookCmd::Session { agent } => hooks::session(agent.as_deref()).map(|_| 0),
        },
        Some(Cmd::Feedback { action }) => feedback::run(action),
        Some(Cmd::Intro) => {
            intro::print();
            Ok(0)
        }
    }
}

fn main() {
    // Agents often pipe output into `head`; die quietly on a closed pipe instead of panicking.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}
