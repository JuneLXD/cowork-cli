# room-cli

`room` is a single static binary that lets two or more AI coding agents (Claude Code,
Codex, others) talk to each other through a shared, locked Markdown log inside a
repository. Every write takes a kernel-level `flock`, so concurrent posts never
corrupt the file. Every read returns only what the caller has not seen yet.

The full design is in [room-cli.md](room-cli.md).

## Install

With Rust on the machine, from a checkout:

```sh
./install.sh            # builds with cargo, installs ~/.local/bin/room
# or
cargo install --path .  # installs ~/.cargo/bin/room
```

Without Rust, copy the static binary. It has no runtime dependencies, so it runs on any
x86_64 Linux or WSL:

```sh
# on a machine with Rust, once:
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
scp target/x86_64-unknown-linux-musl/release/room other-host:~/.local/bin/room
```

Then `room init` in a repository on the other machine. Linux and WSL only.

## Quick start

```sh
cd your-repo
room init
```

That creates `.ai-common/` with a `main` room, writes a rules block into `CLAUDE.md`
and `AGENTS.md`, adds the cursor directory to `.gitignore`, and prints one kickoff
prompt per agent. Open Claude Code in one terminal and Codex in another, paste each
prompt, and they coordinate on their own. Nothing else to configure.

Reprint a prompt any time:

```sh
room prompt claude --copy     # kickoff prompt, copied to the clipboard
room prompt codex --rules     # the persistent rules block
```

## Two surfaces

People use the menu. Agents use the flags.

**Menu.** Type `room` on its own in a terminal and you get a Claude Code style menu:
arrow keys to move, Enter to select, Esc to go back. Before setup it offers to
initialize the repository (or the directory, when there is no git). After setup it lists
the rooms and offers, in order of how often you need them:

1. Create a room. It asks for a name, a purpose, and which agent executes, then prints
   the two role prompts and opens a picker that copies one prompt per selection. The
   picker stays open so you can copy the second one; its last option goes back.
2. Open a room. Stream it live, show recent messages, reprint its role prompts, post a
   note as yourself, read unread as an agent, or delete the room after a confirmation.
3. Show the setup prompt for an agent, run doctor, check the daemon, re-run setup.

When output is piped, bare `room` prints the plain room list instead, so agents and
scripts never hang on a prompt.

**Flags.** Every action is also a subcommand with flags, which is what the prompts tell
the agents to use.

## Rooms and roles

Every room has one executor and one or more advisors. The executor proposes each change,
waits for a vote, and is the only one who edits files. Advisors read, suggest, and vote.
`room new <name> --executor codex` creates the room and prints one prompt per agent.
Each prompt is self-contained: role, working loop, and the exact commands to use, so an
agent can work from the prompt alone. Prompts are saved under `.ai-common/prompts/` and
reprinted with `room prompt <agent> --room <name>`.

Votes are a field on a post: `room post --vote "approve: reason"` or
`--vote "reject: reason"`. They show up as a `Vote` line in the log and in `--json`.

## Everyday commands

```sh
room                    # status of every room in this project
room read               # unread messages for the calling agent
room read --last 3      # last three messages; add --json for structured output
room post --thoughts "..." --action "..." --taken "..." --handoff "..."
room post --thoughts-file - < notes.md     # multi-line content from stdin
room wait --timeout 300 # block until someone else posts (exit 2 on timeout)
room wait --all-rooms   # same, across every room of the project
room hook status        # are the Claude Code / Codex hooks installed?
room new review --purpose "PR review thread" --executor codex
room prompt claude --room review        # role prompt for one agent in that room
room post --room review --thoughts "..." --vote "approve: reason"
room stream --room review               # follow live, ctrl-c stops
room delete review --yes                # remove a room, its cursors, and its prompts
room list               # rooms in this project; --all for every project
room status --json
room archive --keep 20  # move older messages to .ai-common/archive/
room lock plan.md -- sh -c 'echo step >> plan.md'   # any command under an exclusive lock
room doctor             # check PATH, project, agent identity, daemon, clipboard
```

## How the agent is identified

In order: `--agent` (post) or `--me` (read, wait), then `ROOM_AGENT`, then environment
markers set by the tool itself (`CLAUDECODE` for Claude Code, `CODEX_*` for Codex).
From a plain shell, run `ROOM_AGENT=claude room read`.

## Rooms and addressing

A room is one log file at `.ai-common/rooms/<room>.md`. Every project has `main`.
Address a room in another repository as `<project>/<room>`, where the project name is
the repository directory name. `room init` registers each project so this works.

## How messages reach the agents

Neither Claude Code nor Codex can watch a terminal or accept a socket push into a live
session. Both are turn-based: new text enters only through a tool result, a hook, or a
user prompt. room-cli uses all three.

**Hooks (automatic).** `room init` writes three hooks for each tool into
`.claude/settings.json` and `.codex/hooks.json`. Both tools share the same hook contract.

| Event | What `room hook` does |
|---|---|
| `Stop` | When the agent finishes a turn, waits up to `hook_wait` seconds (default 120) for a new message in any room. If one arrives, the agent is kept running with the message as its next instruction. Capped at `hook_max_continues` per session (default 100). |
| `UserPromptSubmit` | Adds unread room messages to the agent's context before it handles your prompt, so it never acts on stale state. |
| `SessionStart` | Briefs the agent on its identity, its rooms, its role in each, unread counts, and the latest handoff. |

Claude Code loads project hooks at the next session start. Codex requires you to trust
them once: type `/hooks` inside Codex and trust the three `room hook` entries. Manage
them with `room hook install`, `room hook remove`, and `room hook status`.

**Blocking wait (in the prompts).** `room wait` blocks on the daemon's inotify signal
and returns the instant the counterpart posts. In Claude Code the prompts say to run it
as a background task, which yields a notification while the agent keeps working, with
no timeout. In Codex the prompts say to loop it with a short timeout.

**Tool-specific instructions.** Every prompt and rules block ends with a section for the
tool in question: Claude Code gets the background-wait and 10-minute timeout guidance,
Codex gets the hook-trust step and the short-loop guidance. Other agent names get a
generic block. `room wait --all-rooms` waits on every room at once.

## Daemon

`room wait` uses a small background watcher for push notification. It is the same
binary, auto-started on first use, exits after an hour idle, and never writes to room
files. If it is not running, `wait` polls once a second instead. `room daemon status`
shows it. Errors from a failed start land in `~/.local/share/room/daemon.log`.

## Layout

```
.ai-common/
  rooms/<room>.md      committed logs
  .cursors/<agent>/    gitignored read positions
  archive/             output of room archive
  templates/           optional overrides: <agent>.md, <agent>.rules.md, generic.md
  PROTOCOL.md          human-readable rules
  ONBOARDING.md        the prompts init printed
  room.toml            only created by room config set
```
