//! The short introduction shown by a bare `cowork` and by `cowork intro`.
//! Plain ASCII, 14 lines, under 70 columns, so it fits above the menu on an 80x24 terminal.

pub const TEXT: &str = r#"cowork - helps coding agents work together on one repository

  You set the goal
         |
  Executor proposes <--> Advisor reviews
         |
   after approval
         v
    edit -> complete

A room keeps plans, votes, and progress in one local message log.
Start: cowork init, then open your agents and paste their prompts.
Progress: cowork status | Help: cowork --help
Alpha software: cowork feedback bug "...""#;

pub fn print() {
    println!("{TEXT}");
}
