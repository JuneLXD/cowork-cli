//! The short introduction shown by a bare `cowork` and by `cowork intro`.
//! Plain ASCII, 14 lines, under 70 columns, so it fits above the menu on an 80x24 terminal.

pub const TEXT: &str = r#"cowork {version} - helps coding agents work together on one repository

  You set the goal
         |
  Executor proposes <--> Advisor reviews
         |
   after approval
         v
    edit -> complete

A room keeps plans, votes, and progress in one local message log.
Start: cowork init [--agents claude,codex,kimi]; paste the prompts.
Progress: cowork status | Help: cowork --help
Alpha software: cowork feedback bug "...""#;

/// The banner with the version filled in (the first line reads `cowork 0.2.0 - ...`).
pub fn text() -> String {
    TEXT.replace("{version}", env!("CARGO_PKG_VERSION"))
}

pub fn print() {
    println!("{}", text());
}
