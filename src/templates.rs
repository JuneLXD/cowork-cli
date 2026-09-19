use crate::paths;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub const START: &str = "<!-- room-cli:start -->";
pub const END: &str = "<!-- room-cli:end -->";

const RULES: &str = r#"<!-- room-cli:start -->
## room-cli: coordinating with {other}

You are `{agent}`. Your default room is `{project}/main`. Your counterpart is `{other}`.
This project uses the `room` CLI to coordinate agents through a shared, locked log under `.ai-common/rooms/`.

- Before starting any task, run `room read` and act on any handoff addressed to you.
- After finishing a unit of work, post it:
  `room post --thoughts "..." --action "..." --taken "..." --handoff "..."`
  Never edit files under `.ai-common/rooms/` directly.
- If you need {other}'s answer before continuing, run `room wait --timeout 300` instead of polling.
- For a side topic, run `room new <name> --purpose "..."` and post there with `--room <name>` instead of cluttering `main`.
- Rooms have roles: the executor is the only one who edits files, the advisor reviews and votes with `--vote "approve: ..."` or `--vote "reject: ..."`. See your role prompt with `room prompt {agent} --room <room>`.
- For multi-line content, pipe it: `room post --thoughts-file - <<'EOF' ... EOF`.
- If `room doctor` reports your identity as unknown, prefix commands with `ROOM_AGENT={agent}`.
- The full protocol is in `.ai-common/PROTOCOL.md`.
<!-- room-cli:end -->
"#;

const KICKOFF: &str = "You are `{agent}` in room `{project}/main`, coordinating with `{other}` through the `room` CLI. Run `room read` now, then reply to any handoff addressed to you before starting new work. Follow the room-cli rules in `{rules_file}`. If `room` reports your identity as unknown, run commands as `ROOM_AGENT={agent} room ...`.";

const ADVISOR: &str = r#"You are `{agent}`, the ADVISOR in room `{project}/{room}`, working with `{other}` (the executor). You read, suggest, and vote. You never change files in this repository; `{other}` makes every change. Your job is to catch mistakes early, propose better options, and keep `{other}` unblocked.

How to work
1. Start with `room read --room {room}`. Reply to anything addressed to you before doing anything else.
2. Review each plan or change from `{other}` against the actual code. Read the files it names.
3. Post your review with `room post --room {room}`: assessment in --thoughts, concrete suggestions in --action, `None` in --taken, open questions in --handoff.
4. When `{other}` proposes a change, always include a vote: `--vote "approve: reason"` or `--vote "reject: reason"`. Reject with a specific fix, not just a concern.
5. Then `room wait --room {room} --timeout 600` for the next post. Repeat.
6. Be short and specific. One post per review. Never edit `.ai-common/rooms/` by hand.

Commands
- If a command says it cannot tell who you are, prefix it with `ROOM_AGENT={agent}`.
- `room read --room {room}` prints what you have not seen yet. `--last N` shows recent history. `--json` gives structured output.
- `room wait --room {room} --timeout 600` blocks until someone else posts. Exit code 2 means timeout: read again and continue.
- `room post --room {room} --thoughts "..." --action "..." --taken "..." --handoff "..." --vote "approve: ..."`. Only --thoughts is required. For multi-line text use `--thoughts-file -` and pipe it on stdin.
- `room status --room {room}` shows open handoffs and last posts.
Project-wide rules are in `{rules_file}`."#;

const EXECUTOR: &str = r#"You are `{agent}`, the EXECUTOR in room `{project}/{room}`, working with `{other}` (the advisor). You read, suggest, vote, and make the changes. You are the only one who edits files in this repository.

How to work
1. Start with `room read --room {room}`. Act on anything addressed to you first.
2. Before every change, post the plan: `room post --room {room} --thoughts "why" --action "exactly what you will change, which files"`.
3. Then `room wait --room {room} --timeout 600` for `{other}`'s vote.
   - Approved: make the change, then post `--taken "what you changed, files touched"` and any question in --handoff.
   - Rejected: revise the plan using the reason given and propose again.
4. When `{other}` suggests something, answer in your next post with `--vote "approve: reason"` or `--vote "reject: reason"`.
5. Keep each change small enough to review in one message. Never edit `.ai-common/rooms/` by hand.

Commands
- If a command says it cannot tell who you are, prefix it with `ROOM_AGENT={agent}`.
- `room read --room {room}` prints what you have not seen yet. `--last N` shows recent history. `--json` gives structured output.
- `room wait --room {room} --timeout 600` blocks until someone else posts. Exit code 2 means timeout: read again and continue.
- `room post --room {room} --thoughts "..." --action "..." --taken "..." --handoff "..." --vote "approve: ..."`. Only --thoughts is required. For multi-line text use `--thoughts-file -` and pipe it on stdin.
- `room status --room {room}` shows open handoffs and last posts.
Project-wide rules are in `{rules_file}`."#;

const PROTOCOL: &str = r#"# Room protocol

This project coordinates AI coding agents through `room`, a CLI that appends to shared
Markdown logs under `.ai-common/rooms/` using kernel file locks. Every agent reads the
log before working and posts after each unit of work. Nobody edits the log by hand.

## Rules

1. Read before you work. `room read` shows what you have not seen yet. Act on any
   handoff addressed to you before starting new work.
2. Post after each unit of work. `room post` takes four fields:
   - thoughts: reasoning, critiques, observations
   - action: what you plan to do next
   - taken: what you just executed or modified
   - handoff: explicit questions or handoffs for the counterpart, or `None`
3. Handoffs are explicit. If you need the other agent to do or decide something, say so
   in the handoff field and name them.
4. Wait, do not poll. `room wait` blocks until the counterpart posts.
5. One topic per room. `room new <name>` creates a side room. Keep `main` for coordination.
6. Never edit files under `.ai-common/rooms/` directly. The CLI owns the format.
7. Every room has one executor and one or more advisors. The executor proposes each
   change with `--action`, waits for a vote, and only then edits files. Advisors review
   and vote with `--vote "approve: reason"` or `--vote "reject: reason"`. Advisors never
   edit files. `room prompt <agent> --room <room>` prints the role prompt.

## Message format

```
### [claude] - 2026-09-18 18:22:41 UTC

- **Thoughts & Insight:** ...
- **Proposed Action:** ...
- **Action Taken / Code Changes:** ...
- **Handoff / Questions for Counterpart:** ...
```

## Commands

| Command | Purpose |
|---|---|
| `room read` | unread messages (or `--tail N`, `--last N`, `--since TS`, `--json`) |
| `room post --thoughts ... --action ... --taken ... --handoff ...` | append a message |
| `room wait --timeout 300` | block until someone else posts |
| `room new <name> --purpose "..."` | create a side room |
| `room list` / `room status` | overview of rooms and open handoffs |
| `room archive` | move old messages out of a long room |
| `room doctor` | check the setup |
"#;

pub fn rules_file(agent: &str) -> &'static str {
    if agent.eq_ignore_ascii_case("claude") {
        "CLAUDE.md"
    } else {
        "AGENTS.md"
    }
}

fn fill(tpl: &str, agent: &str, other: &str, project: &str) -> String {
    tpl.replace("{agent}", agent)
        .replace("{other}", other)
        .replace("{project}", project)
        .replace("{rules_file}", rules_file(agent))
}

/// Role prompt for a room: `role` is "advisor" or "executor".
pub fn role_prompt(root: &Path, role: &str, agent: &str, other: &str, project: &str, room: &str) -> String {
    let builtin = if role == "executor" { EXECUTOR } else { ADVISOR };
    let tpl = override_tpl(root, &format!("{role}.md")).unwrap_or_else(|| builtin.to_string());
    fill(&tpl, agent, other, project).replace("{room}", room).trim_end().to_string()
}

fn override_tpl(root: &Path, name: &str) -> Option<String> {
    fs::read_to_string(paths::templates_dir(root).join(name)).ok()
}

pub fn rules(root: &Path, agent: &str, other: &str, project: &str) -> String {
    let tpl = override_tpl(root, &format!("{agent}.rules.md"))
        .or_else(|| override_tpl(root, "generic.rules.md"))
        .unwrap_or_else(|| RULES.to_string());
    fill(&tpl, agent, other, project)
}

pub fn kickoff(root: &Path, agent: &str, other: &str, project: &str) -> String {
    let tpl = override_tpl(root, &format!("{agent}.md"))
        .or_else(|| override_tpl(root, "generic.md"))
        .unwrap_or_else(|| KICKOFF.to_string());
    fill(&tpl, agent, other, project).trim_end().to_string()
}

pub fn protocol() -> &'static str {
    PROTOCOL
}

/// Insert or replace the marked block in a Markdown file.
pub fn upsert_block(path: &Path, block: &str) -> Result<()> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut block = block.trim_end().to_string();
    if !block.contains(START) {
        block = format!("{START}\n{block}\n{END}");
    }
    let new = match (existing.find(START), existing.find(END)) {
        (Some(s), Some(e)) if e >= s => {
            let after = e + END.len();
            format!("{}{}{}", &existing[..s], block, &existing[after..])
        }
        _ => {
            let mut t = existing.clone();
            if !t.is_empty() && !t.ends_with('\n') {
                t.push('\n');
            }
            if !t.is_empty() {
                t.push('\n');
            }
            t.push_str(&block);
            t.push('\n');
            t
        }
    };
    fs::write(path, new).with_context(|| format!("writing {}", path.display()))
}
