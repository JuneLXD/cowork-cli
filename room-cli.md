# room-cli

## Overview

room-cli is a lightweight, zero-dependency synchronization tool that lets two or more autonomous coding agents (Claude Code, OpenAI Codex, others) communicate concurrently on the same repository without race conditions or log corruption.

Instead of letting agents overwrite a shared markdown document at the same time, room-cli acts as an atomic message broker and context filter backed by Linux kernel-level advisory file locking (flock). Conversations are organized into rooms, each scoped to a project, so several agent pairs or several topics can run side by side.

Design goals, in priority order:

1. Atomic, ordered writes. Two agents posting at once never corrupt or clobber the log.
2. Out-of-the-box usable. One binary, one `room init`, paste two prompts, done. No config file required.
3. Cheap context. Agents read only what is new, never the whole log.
4. Degrades gracefully. Everything works without the daemon. The daemon only adds push notifications and cross-project views.

Non-goals for v1: cross-platform support (Linux and WSL only), network-shared rooms, rooms spanning two repositories.

## Core architecture

```
┌───────────────────────────┐
│ Developer / You           │
└─────────────┬─────────────┘
              │ room init, then starts both tools
┌─────────────┴─────────────────────┐
▼                                   ▼
┌──────────────────┐        ┌──────────────────┐
│ Claude Code      │        │ Codex Agent      │
└─────────┬────────┘        └─────────┬────────┘
          │ room post / read / wait   │
          └─────────────┬─────────────┘
                        ▼
          ┌─────────────────────────┐
          │ room CLI (Rust binary)  │
          │ - Detects calling agent │
          │ - Acquires OS flock     │
          │ - Formats timestamp     │
          │ - Atomically appends    │
          │ - Reads unread / tail   │
          └────────────┬────────────┘
                       ▼
          ┌─────────────────────────┐
          │ .ai-common/rooms/*.md   │      ┌───────────────────────┐
          │ (one file per room)     │◄─────│ room daemon (optional)│
          └─────────────────────────┘      │ - inotify watcher     │
                                           │ - serves wait/status  │
                                           │ - project registry    │
                                           └───────────────────────┘
```

The CLI is the only writer. The daemon watches, it never writes to room files.

## Key problems it solves

- Race conditions and log clobbering. An exclusive advisory flock on the file descriptor makes concurrent writes queue cleanly.
- Format compliance. Models vary headers and omit timestamps. The CLI accepts structured fields and generates the Markdown itself.
- Context window bloat. `read` returns only unread messages, or the last N messages or lines, never the whole history.
- Topic sprawl. Separate rooms keep a migration discussion out of the main thread, and let a second agent pair work on the same repo.
- Polling waste. `wait` blocks until the counterpart posts, so an agent can hand off and sleep instead of re-reading in a loop.
- Onboarding friction. `init` generates the persistent rules for each tool and a paste-ready kickoff prompt.

## Technical specification

- Language: Rust, single static binary. Build for the `x86_64-unknown-linux-musl` target so it runs on any Linux without glibc concerns.
- Crates: `clap` (derive) for CLI parsing, `inquire` for the interactive menu, `fs2` or `libc::flock` for locking, `chrono` for UTC timestamps, `notify` (inotify) for the daemon watcher, `serde` + `toml` for the optional config and registry.
- Timestamp format: `YYYY-MM-DD HH:MM:SS UTC`. Seconds are required so two posts in the same minute stay distinguishable.
- Block delimiter: every message starts with `### [` at column zero. Agent names may not contain `]` or newlines. `post` rejects them.
- Install location: `~/.local/bin/room`. `cargo install --path .` during development. Later, `curl -sSf <url>/install.sh | sh` which drops the binary in `~/.local/bin` and warns if that is not on PATH.

## Concepts

### Project

A project is a git repository. The project name defaults to the git top-level directory name and can be overridden with `room config set name <NAME>`. `room init` refuses to run outside a git repo unless `--no-git` is passed.

### Room

A room is one conversation log inside a project, stored at `.ai-common/rooms/<room>.md`. Every project has a `main` room created by `init`. Additional rooms are topics such as `auth-migration` or `review`.

A room belongs to exactly one project. Agents in another project can post into it by address, but a room never spans two repositories.

Full address form: `<project>/<room>`. Inside a project, the bare `<room>` form resolves against the current project. With no `--room` flag at all, the room is `main`.

### Agent identity

The CLI resolves the calling agent in this order:

1. `--agent <NAME>` flag.
2. `ROOM_AGENT` environment variable.
3. Tool detection from environment markers: `CLAUDECODE=1` means `claude`, the Codex marker means `codex`.
4. Error with a hint to set `ROOM_AGENT`.

Default participants are `claude` and `codex`. The counterpart is whoever is not me. `room init --agents a,b,c` sets a different list, stored in `room.toml`.

### Cursor

Each agent has a per-room cursor at `.ai-common/.cursors/<agent>/<room>`, recording the last message it has read. Cursors are gitignored. They drive `read --unread` and `wait`.

## Storage layout

```
<repo>/
  .ai-common/
    rooms/
      main.md                 # room log, committed
      auth-migration.md
    .cursors/                 # gitignored
      claude/main
      codex/main
    templates/                # optional, overrides built-in prompt templates
      claude.md
      codex.md
      generic.md
    archive/                  # created by `room archive`
      main-20260918.md
    PROTOCOL.md               # human-readable rules, written by init
    ONBOARDING.md             # the prompts printed by init, for re-reading
    prompts/<room>.md         # role prompts printed by `room new`
    room.toml                 # optional, only created when a setting is changed
  CLAUDE.md                   # gets a marked block from init
  AGENTS.md                   # gets a marked block from init

~/.local/share/room/
  projects.toml               # registry: project name -> absolute path, maintained by init and the daemon

$XDG_RUNTIME_DIR/room.sock    # daemon socket
```

### Room file format

Each room file begins with a small front-matter block, then messages. A `Vote` line appears only on posts that carry one:

```markdown
---
room: main
project: landing
created: 2026-09-18 18:20:00 UTC
purpose: General coordination
participants: claude, codex
executor: claude
---

### [claude] - 2026-09-18 18:22:41 UTC

- **Thoughts & Insight:** Evaluated the database design; adding an index on user_id will avoid scan bottlenecks.
- **Proposed Action:** Update migrations/001_auth.sql.
- **Action Taken / Code Changes:** None yet.
- **Handoff / Questions for Counterpart:** codex, do you agree with this migration index, or would you prefer a compound index?
```

## CLI reference

### `room` (no subcommand)

The menu is the human surface; agents use the flag subcommands. On a terminal it is an interactive menu in the style of Claude Code's prompts (arrow keys, Enter, Esc). In an uninitialized directory it offers to set up the repository, or the bare directory when no git repository is found, and explains what setup does. In an initialized project it lists the rooms and offers, ordered by how often a person needs them: create a room (name, purpose, choose the executor, print both role prompts, then a picker that copies one prompt per selection and stays open until its last option, Back, is chosen); open a room, which leads to a room submenu with stream live, recent messages, role prompts, post a note as `human`, read unread as an agent, and delete the room after confirmation; show the setup prompt for an agent; doctor; daemon status; re-run setup.

When stdin or stdout is not a terminal: prints the status and the `room prompt` hints, or a one-line "run `room init`" note, so agents and scripts never block on a prompt.

`room init` outside a git repository also asks for confirmation on a terminal instead of failing; non-interactively it still requires `--no-git`.

### `room init`

Scaffolds everything and prints the onboarding prompts. Idempotent. Steps:

1. Create `.ai-common/rooms/main.md` with front matter.
2. Write `PROTOCOL.md`.
3. Append a block to `CLAUDE.md` and `AGENTS.md` between `<!-- room-cli:start -->` and `<!-- room-cli:end -->` markers. Re-running updates the block in place instead of appending a duplicate.
4. Add `.ai-common/.cursors/` to `.gitignore`.
5. Register the project in `~/.local/share/room/projects.toml`.
6. Write `ONBOARDING.md` and print the kickoff prompt for each participant.

Flags: `--agents <a,b,c>` (default `claude,codex`), `--no-git`, `--quiet` (skip printing prompts).

### `room post`

Acquires an exclusive lock on the room file, generates a timestamped header, appends the structured content, flushes, unlocks, and advances the poster's own cursor.

Flags:

- `--room <ADDR>` (default `main`)
- `--agent <NAME>` (default: detected)
- `--thoughts <TEXT>` (required unless `--thoughts-file`)
- `--action <TEXT>` (alias `--proposed-action`)
- `--taken <TEXT>` (alias `--action-taken`)
- `--handoff <TEXT>`
- `--vote <TEXT>`: `approve: reason`, `reject: reason`, or `abstain: reason`. Rendered as a fifth `Vote` line only when present.
- `--thoughts-file <PATH>`, `--action-file`, `--taken-file`, `--handoff-file`. Pass `-` for stdin. For multi-line content without shell escaping.

Omitted fields are written as `None`, so a quick note needs only `--thoughts`.

### `room read`

Acquires a shared lock and prints messages.

- No flags: unread messages for the calling agent. If the agent has no cursor yet, falls back to the last 40 lines. Advances the cursor after printing.
- `--unread`: same as default, explicit.
- `--tail <LINES>`: last N lines.
- `--last <COUNT>`: last N message blocks, parsed on the `### [` delimiter.
- `--since <TIMESTAMP>`: blocks at or after the given time.
- `--agent <NAME>`: only blocks from that agent.
- `--room <ADDR>`.
- `--me <NAME>`: identity of the calling agent, since `--agent` is the filter here.
- `--json`: emit an array of `{agent, timestamp, thoughts, action, taken, handoff}` objects instead of Markdown.
- `--no-advance`: do not move the cursor.

### `room wait`

Blocks until a new post from someone other than the calling agent lands in the room, then prints it (same output options as `read`) and exits 0. Exits 2 on timeout.

Flags: `--room <ADDR>`, `--timeout <SECONDS>` (default 300), `--me <NAME>`, `--json`.

Uses the daemon for push notification when available, otherwise falls back to polling the file every second.

### `room new <room>`

Creates a room file with front matter, assigns roles, prints one role prompt per agent, and saves them to `.ai-common/prompts/<room>.md`. Flags: `--purpose <TEXT>`, `--executor <AGENT>` (default: first participant).

Roles: exactly one executor per room, who proposes each change, waits for a vote, and is the only agent that edits files. Every other participant is an advisor who reads, suggests, and votes. The executor is stored in the room's front matter.

Role prompts are self-contained. Each one states the role, the working loop (read, propose or review, vote, wait), and the exact commands with flags, so an agent can operate from the prompt alone without having read `CLAUDE.md` or `AGENTS.md`. Built-in templates can be overridden by `.ai-common/templates/advisor.md` and `executor.md`, with the extra `{room}` placeholder.

### `room delete <room>` (alias `rm`)

Removes the room log, every agent's cursor for it, and its saved prompts. Archives under `.ai-common/archive/` are kept. On a terminal it asks for confirmation showing the message count; non-interactively it refuses without `--yes`. `main` cannot be deleted, since it is the default room; `room archive --room main` clears it instead.

### `room stream`

Prints the last N messages of a room, then follows it live, printing each new post as it lands (daemon push, or polling when the daemon is unavailable). Ctrl-c stops it. Flags: `--room <ADDR>`, `--last <N>` (default 5), `--json`.

### `room list`

Lists rooms in the current project with message count, last poster, and last timestamp. `--all` lists every registered project and its rooms via the daemon or registry.

### `room status`

Last post per agent, open handoffs (handoff field not `None` in the latest block per agent), total message count. `--json` supported. `--all` spans projects.

### `room prompt <tool>`

Prints the kickoff prompt for an agent. `--room <ROOM>` prints that agent's role prompt for the room instead. `--rules` prints the persistent block. `--copy` pipes the result to `clip.exe` on WSL or `xclip` elsewhere.

### `room lock <path> -- <command>`

Runs an arbitrary command while holding an exclusive flock on `<path>`. Extends the race-condition guarantee to plan files, task lists, or any shared scratch file.

### `room archive`

Moves all but the last N blocks (default 20) of a room to `.ai-common/archive/<room>-<YYYYMMDD>.md` and leaves a one-line stub at the top of the room noting the archive. Flags: `--room`, `--keep <N>`.

### `room config`

`room config set <key> <value>`, `room config get <key>`, `room config list`. Creates `room.toml` on first `set`. Keys: `name`, `agents`, `tail`, `wait_timeout`, `archive_keep`.

### `room daemon start|stop|status`

Manual control for the rare case. Normally never needed.

### `room doctor`

One line per check with a fix hint: binary on PATH, project initialized, git repo detected, daemon socket responds, agent markers recognized, CLAUDE.md and AGENTS.md blocks present.

## Daemon

The daemon is the same binary run as `room daemon`. It never writes to room files. Its job is everything flock cannot do.

- One instance per user, listening on `$XDG_RUNTIME_DIR/room.sock` (fallback `~/.local/share/room/room.sock`, or `/tmp/room-<uid>.sock` when that path exceeds the Unix socket length limit). Its stderr goes to `~/.local/share/room/daemon.log`.
- Auto-started by any command that needs it if the socket is missing. Exits after one hour idle.
- Uses inotify on every room file of every registered project.
- Serves: `wait` push notifications, `subscribe` streaming, `status --all`, `list --all`, and cross-project room address resolution.
- Maintains `projects.toml`, adding entries when it sees a new project via `init` or a cross-project address.

If the daemon is unreachable, every command still works. `wait` polls, `--all` reads the registry file directly, and cross-project addresses resolve from the registry.

## Onboarding prompts

`init` and `prompt` generate two pieces of text per tool, from built-in templates that can be overridden by files in `.ai-common/templates/`.

### Persistent rules block

Written into `CLAUDE.md` for Claude Code and `AGENTS.md` for Codex, which each tool reads automatically. Content:

- Your identity is `<agent>`. Your default room is `<project>/main`. Your counterpart is `<other>`.
- Before starting any task, run `room read` and act on open handoffs addressed to you.
- After finishing a unit of work, run `room post` with thoughts, action, taken, and handoff. Never edit files under `.ai-common/rooms/` directly.
- If you need the counterpart's answer, run `room wait --timeout N` instead of polling.
- For a side topic, run `room new <name>` and post there rather than cluttering `main`.

### Kickoff prompt

Short, pasted once into a fresh session:

> You are `<agent>` in room `<project>/main`. Run `room read`, then reply to any handoff addressed to you before starting new work. Your counterpart is `<other>`. Follow the room-cli rules in `<CLAUDE.md|AGENTS.md>`.

## Defaults and configuration

There is no required configuration. Every setting has a default and `room.toml` is only created by `room config set`.

| Setting | Default | Source |
|---|---|---|
| project name | git top-level dir name | `config set name` |
| room | `main` | `--room`, `ROOM_ID` |
| agent | detected from tool markers | `--agent`, `ROOM_AGENT` |
| agents list | `claude,codex` | `init --agents`, `config set agents` |
| tail lines | 40 | `config set tail` |
| wait timeout | 300 s | `--timeout`, `config set wait_timeout` |
| archive keep | 20 blocks | `--keep`, `config set archive_keep` |

## First-run flow

```
cargo install --path .        # or the install script
cd my-repo
room init                     # prints two kickoff prompts
# terminal 1: claude, paste the claude prompt
# terminal 2: codex,  paste the codex prompt
```

Nothing else. Both agents now read unread messages before working and post after each unit of work.

## Known trade-offs

- Tool detection from environment markers could drift if a tool renames its variable. `ROOM_AGENT` is the fallback and `room doctor` shows the detected value.
- flock is per-machine. Rooms on a network mount or shared between two hosts lose the ordering guarantee. Out of scope for v1.
- Cursors are per checkout, not per clone. Two checkouts of the same repo have independent cursors, which is the intended behavior.

## Build order

Each step is usable on its own before moving to the next.

1. Core: `post`, `read --tail/--last/--since/--agent/--json`, flock, timestamps with seconds, `### [` parsing. Single `rooms/main.md`.
2. Rooms and cursors: `--room`, `new`, `list`, `read --unread` as default, `ROOM_AGENT` and tool detection.
3. Onboarding: `init` with marked blocks in CLAUDE.md and AGENTS.md, `PROTOCOL.md`, `ONBOARDING.md`, `prompt --copy`, bare `room` status, `doctor`.
4. Convenience: `status`, `lock`, `archive`, `config`, `wait` with polling fallback.
5. Daemon: socket, inotify, auto-start, `wait` push, `--all` views, cross-project addressing, `projects.toml`.
6. Packaging: musl build, `install.sh`.
