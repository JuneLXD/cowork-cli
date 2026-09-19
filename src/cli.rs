use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "room",
    version,
    about = "Atomic, locked message rooms for coordinating AI coding agents on one repository",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Scaffold .ai-common/, rules blocks, and print kickoff prompts
    Init {
        /// Comma-separated participant names (default: claude,codex)
        #[arg(long)]
        agents: Option<String>,
        /// Initialize even outside a git repository
        #[arg(long)]
        no_git: bool,
        /// Do not print the kickoff prompts
        #[arg(long)]
        quiet: bool,
    },
    /// Append a structured message to a room
    Post(PostArgs),
    /// Print unread messages (default), or a slice of the room
    Read(ReadArgs),
    /// Block until someone else posts in the room
    Wait(WaitArgs),
    /// Create a room and print its two role prompts (advisor and executor)
    New {
        /// Room name (letters, digits, - _ .)
        name: String,
        #[arg(long)]
        purpose: Option<String>,
        /// The agent that writes changes in this room (default: first participant)
        #[arg(long, value_name = "AGENT")]
        executor: Option<String>,
    },
    /// Delete a room, its cursors, and its saved prompts (asks first on a terminal)
    #[command(alias = "rm")]
    Delete {
        /// Room name
        name: String,
        /// Do not ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Follow a room live, printing new messages as they arrive (ctrl-c stops)
    Stream {
        #[arg(long)]
        room: Option<String>,
        /// How many recent messages to show first
        #[arg(long, default_value_t = 5)]
        last: usize,
        #[arg(long)]
        json: bool,
    },
    /// List rooms in this project, or every registered project with --all
    List {
        #[arg(long)]
        all: bool,
    },
    /// Last post per agent, open handoffs, and message counts
    Status {
        /// Only this room
        #[arg(long)]
        room: Option<String>,
        /// Every registered project
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print the kickoff prompt (or rules block, or a room's role prompt) for an agent
    Prompt {
        /// Agent name, e.g. claude or codex
        agent: String,
        /// Print the role prompt (advisor or executor) for this room instead
        #[arg(long, value_name = "ROOM")]
        room: Option<String>,
        /// Print the persistent rules block instead of the kickoff prompt
        #[arg(long)]
        rules: bool,
        /// Also copy to the clipboard (clip.exe on WSL, xclip, or wl-copy)
        #[arg(long)]
        copy: bool,
    },
    /// Run a command while holding an exclusive lock on a file
    Lock {
        /// File to lock (created if missing)
        path: String,
        /// Command to run, after `--`
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Move old messages of a room into .ai-common/archive/
    Archive {
        #[arg(long)]
        room: Option<String>,
        /// Number of most recent messages to keep (default 20)
        #[arg(long)]
        keep: Option<usize>,
    },
    /// Read or change optional settings in .ai-common/room.toml
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Control the background watcher (normally auto-started)
    Daemon {
        #[command(subcommand)]
        action: DaemonCmd,
    },
    /// Check the installation and project setup
    Doctor,
}

#[derive(Args)]
pub struct PostArgs {
    /// Room address: <room> or <project>/<room> (default: main)
    #[arg(long)]
    pub room: Option<String>,
    /// Posting agent (default: detected from the environment)
    #[arg(long, alias = "me")]
    pub agent: Option<String>,
    /// Reasoning, critiques, or observations
    #[arg(long)]
    pub thoughts: Option<String>,
    /// Planned tasks or file changes
    #[arg(long, alias = "proposed-action")]
    pub action: Option<String>,
    /// What was just executed or modified
    #[arg(long, alias = "action-taken")]
    pub taken: Option<String>,
    /// Questions or handoffs for the counterpart
    #[arg(long)]
    pub handoff: Option<String>,
    /// Vote on the counterpart's proposal: approve, reject, or abstain, plus a reason
    #[arg(long, value_name = "TEXT")]
    pub vote: Option<String>,
    /// Read thoughts from a file, or `-` for stdin
    #[arg(long, value_name = "PATH")]
    pub thoughts_file: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub action_file: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub taken_file: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub handoff_file: Option<String>,
}

#[derive(Args)]
pub struct ReadArgs {
    /// Room address: <room> or <project>/<room> (default: main)
    #[arg(long)]
    pub room: Option<String>,
    /// Only unread messages for the calling agent (this is the default)
    #[arg(long)]
    pub unread: bool,
    /// Last N lines of the raw file
    #[arg(long, value_name = "LINES")]
    pub tail: Option<usize>,
    /// Last N message blocks
    #[arg(long, value_name = "COUNT")]
    pub last: Option<usize>,
    /// Messages at or after this timestamp (YYYY-MM-DD HH:MM:SS UTC)
    #[arg(long, value_name = "TIMESTAMP")]
    pub since: Option<String>,
    /// Only messages from this agent
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
    /// Identity of the calling agent (default: detected)
    #[arg(long, value_name = "NAME")]
    pub me: Option<String>,
    /// Emit JSON instead of Markdown
    #[arg(long)]
    pub json: bool,
    /// Do not advance the read cursor
    #[arg(long)]
    pub no_advance: bool,
}

#[derive(Args)]
pub struct WaitArgs {
    #[arg(long)]
    pub room: Option<String>,
    /// Seconds to wait before giving up (exit code 2)
    #[arg(long)]
    pub timeout: Option<u64>,
    /// Identity of the calling agent (default: detected)
    #[arg(long, value_name = "NAME")]
    pub me: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// Set a key (name, agents, tail, wait_timeout, archive_keep)
    Set { key: String, value: String },
    /// Print a key
    Get { key: String },
    /// Print all keys with their effective values
    List,
}

#[derive(Subcommand)]
pub enum DaemonCmd {
    /// Start the daemon if it is not running
    Start,
    /// Stop a running daemon
    Stop,
    /// Show whether the daemon is running
    Status,
    /// Run the daemon in the foreground (used by auto-start)
    #[command(hide = true)]
    Run,
}
