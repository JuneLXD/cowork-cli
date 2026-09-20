# cowork

`cowork` is a single static binary that lets two or more AI coding agents (Claude Code,
Codex, others) talk to each other through a shared, locked Markdown log inside a
repository. Every write takes a kernel-level `flock`, so concurrent posts never
corrupt the file. Every read returns only what the caller has not seen yet.

The full design is in [docs/design.md](docs/design.md); the layout of this repository is in [docs/layout.md](docs/layout.md).

## Install

With Rust on the machine, from a checkout:

```sh
./scripts/install.sh    # builds with cargo, installs ~/.local/bin/cowork (and the room alias)
# or
cargo install --path . --force  # installs ~/.cargo/bin/cowork and room; --force also upgrades in place
```

Without Rust, copy the static binary. It has no runtime dependencies, so it runs on any
x86_64 Linux or WSL:

```sh
# on a machine with Rust, once:
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
scp target/x86_64-unknown-linux-musl/release/cowork other-host:~/.local/bin/cowork
```

The static build needs a musl C toolchain on the build machine for the TLS library
(`sudo apt install musl-tools` on Debian or Ubuntu). It is a build dependency only.

Then `cowork init` in a repository on the other machine. Linux and WSL only.

### Feedback endpoint

`cowork feedback` needs to know where reports go. At build time, `build.rs` reads
`COWORK_FEEDBACK_URL` and `COWORK_FEEDBACK_KEY` from the environment, or `project_ID` and
`publishable_key` from a `.env` file next to `Cargo.toml`, and bakes them into the
binary. Only a publishable (insert-only) key is accepted; anything else is dropped. A
build without either source still works, and `cowork feedback` then explains how to set an
endpoint at runtime: `cowork config set feedback_url ...` and `feedback_key ...`, or the
two environment variables. The table schema is in `supabase/feedback.sql`.

## Quick start

```sh
cd your-repo
cowork init
```

That creates `.ai-common/` with a `main` room, writes a rules block into `CLAUDE.md`
and `AGENTS.md`, adds the cursor directory to `.gitignore`, and prints one kickoff
prompt per agent. Open Claude Code in one terminal and Codex in another, paste each
prompt, and they coordinate on their own. Nothing else to configure.

Reprint a prompt any time:

```sh
cowork prompt claude --copy     # kickoff prompt, copied to the clipboard
cowork prompt codex --rules     # the persistent rules block
```

## Two surfaces

People use the menu. Agents use the flags.

**Menu.** Type `cowork` on its own in a terminal and you get a Claude Code style menu:
arrow keys to move, Enter to select, Esc to go back. Before setup it offers to
initialize the repository (or the directory, when there is no git). After setup it lists
the rooms and offers, in order of how often you need them:

1. Create a room. It asks for a name, a purpose, and which agent executes, then prints
   the two role prompts and opens a picker that copies one prompt per selection. The
   picker stays open so you can copy the second one; its last option goes back.
2. Open a room. Stream it live, show recent messages, reprint its role prompts, post a
   note as yourself, read unread as an agent, or delete the room after a confirmation.
3. Show the setup prompt for an agent, run doctor, check the daemon, re-run setup.

When output is piped, bare `cowork` prints the plain cowork list instead, so agents and
scripts never hang on a prompt.

**Flags.** Every action is also a subcommand with flags, which is what the prompts tell
the agents to use.

## Rooms and roles

Every room has one executor and one or more advisors. The executor proposes each change,
waits for a vote, and is the only one who edits files. Advisors read, suggest, and vote.
`cowork new <name> --executor codex` creates the room and prints one prompt per agent.
Each prompt is self-contained: role, working loop, and the exact commands to use, so an
agent can work from the prompt alone. Prompts are saved under `.ai-common/prompts/` and
reprinted with `cowork prompt <agent> --room <name>`.

Every message has a per-room id. The executor posts a plan with `--propose`; the advisor
decides it with `--re <id> --vote "approve: reason"` or `"reject: reason"`; the executor
closes it with `--complete --re <id>`. Only a vote from another participant on the
proposal id counts, and only the latest vote per voter; everything else is shown as
informational. `cowork status` lists every open proposal with who still has to vote, and
`cowork archive` never moves an open proposal thread. A post needs `--thoughts` unless it
carries `--vote`, `--taken`, or `--complete`. Readers see only fields with content; the
log keeps every field.

## Everyday commands

```sh
room                    # status of every room in this project
cowork read               # unread messages for the calling agent
cowork read --last 3      # last three messages; add --json for structured output
cowork read --brief       # one line per message: id, agent, age, marker, first words
cowork post --thoughts "..." --action "..." --taken "..." --handoff "..."
cowork post --propose --thoughts "why" --action "what"     # a plan that needs a vote; prints its id
cowork post --re 12 --vote "approve: reason"               # the decision on proposal #12
cowork post --re 12 --taken "..." --complete               # close #12 when the work is done
cowork post --thoughts-file - < notes.md     # multi-line content from stdin
cowork wait --timeout 300 # block until someone else posts (exit 2 on timeout)
cowork wait --all-rooms   # same, across every room of the project
cowork hook status        # are the Claude Code / Codex hooks installed?
cowork new review --purpose "PR review thread" --executor codex
cowork prompt claude --room review        # role prompt for one agent in that room
cowork post --room review --thoughts "..." --vote "approve: reason"
cowork stream --room review               # follow live, ctrl-c stops
cowork delete review --yes                # remove a room, its cursors, and its prompts
cowork list               # rooms in this project; --all for every project
cowork status --json
cowork archive --keep 20  # move older messages to .ai-common/archive/
cowork lock plan.md -- sh -c 'echo step >> plan.md'   # any command under an exclusive lock
cowork doctor             # check PATH, project, agent identity, daemon, clipboard
cowork feedback bug "..."         # report a bug to the maintainers (alpha software)
cowork feedback advice "..."      # send a suggestion; --file -, --context, --project are optional
cowork feedback retry             # resend reports that were queued while offline
```

## Feedback (alpha)

cowork is alpha software, and every prompt tells the agents so. Anyone, human or
agent, can send a bug report or a suggestion:

```sh
cowork feedback bug "wait returned exit 2 although codex had posted"
cowork feedback advice --file - <<'EOF'
status should list pending proposals
EOF
```

A report is sent only when the command runs; no other command touches the network. The
default payload is the kind, the text, the CLI version, a coarse OS name (linux or wsl),
and who reported (claude, codex, or human). `--project` adds the project name, `--room`
adds a room name, `--context` attaches text you choose. `--dry-run` prints the payload
instead of sending it. Reports go to an insert-only table; the key in the binary cannot
read them back. If sending fails, the report is queued under the data directory and
`cowork feedback retry` resends it later. `cowork feedback list` and `cowork feedback status`
show the queue and the endpoint.

## How the agent is identified

In order: `--agent` (post) or `--me` (read, wait), then `COWORK_AGENT`, then environment
markers set by the tool itself (`CLAUDECODE` for Claude Code, `CODEX_*` for Codex).
From a plain shell, run `COWORK_AGENT=claude cowork read`.

## Rooms and addressing

A room is one log file at `.ai-common/rooms/<room>.md`. Every project has `main`.
Address a room in another repository as `<project>/<room>`, where the project name is
the repository directory name. `cowork init` registers each project so this works.

## How messages reach the agents

Neither Claude Code nor Codex can watch a terminal or accept a socket push into a live
session. Both are turn-based: new text enters only through a tool result, a hook, or a
user prompt. cowork uses all three.

**Hooks (automatic).** `cowork init` writes three hooks for each tool into
`.claude/settings.json` and `.codex/hooks.json`. Both tools share the same hook contract.

| Event | What `cowork hook` does |
|---|---|
| `Stop` | When the agent finishes a turn, waits up to `hook_wait` seconds (default 120) for a new message in any room. If one arrives, the agent is kept running with the message as its next instruction. Capped at `hook_max_continues` per session (default 100). |
| `UserPromptSubmit` | Adds unread room messages to the agent's context before it handles your prompt, so it never acts on stale state. |
| `SessionStart` | Briefs the agent on its identity, its rooms, its role in each, unread counts, and the latest handoff. |

Claude Code loads project hooks at the next session start. Codex requires you to trust
them once: type `/hooks` inside Codex and trust the three `cowork hook` entries. Manage
them with `cowork hook install`, `cowork hook remove`, and `cowork hook status`.

**Blocking wait (in the prompts).** `cowork wait` blocks on the daemon's inotify signal
and returns the instant the counterpart posts. In Claude Code the prompts say to run it
as a background task, which yields a notification while the agent keeps working, with
no timeout. In Codex the prompts say to loop it with a short timeout.

**Tool-specific instructions.** Every prompt and rules block ends with a section for the
tool in question: Claude Code gets the background-wait and 10-minute timeout guidance,
Codex gets the hook-trust step and the short-loop guidance. Other agent names get a
generic block. `cowork wait --all-rooms` waits on every room at once.

## Daemon

`cowork wait` uses a small background watcher for push notification. It is the same
binary, auto-started on first use, exits after an hour idle, and never writes to room
files. If it is not running, `wait` polls once a second instead. `cowork daemon status`
shows it. Errors from a failed start land in `~/.local/share/room/daemon.log`.

## Compatibility with the old name

- `room` is installed next to `cowork` and is the same program, so prompts, hook files,
  and shell habits from before the rename keep working. `cowork init` rewrites the rules
  blocks and hook entries to the new name, and `cowork hook install` the hook entries
  alone; both replace only their own entries and leave other hooks and text untouched.
- Environment variables use the `COWORK_` prefix: `COWORK_AGENT`, `COWORK_ROOM`,
  `COWORK_FEEDBACK_URL` and `COWORK_FEEDBACK_KEY` (set together), `COWORK_FEEDBACK_TIMEOUT`,
  `COWORK_INSTALL_DIR`. The old names (`ROOM_AGENT`, `ROOM_ID`, `ROOM_FEEDBACK_*`,
  `ROOM_INSTALL_DIR`) are read when the new ones are absent; a half-set pair is an error.
- Persistent state keeps its original paths on purpose: `~/.local/share/room/` (registry,
  feedback queue, daemon log), `room.sock`, and `.ai-common/room.toml`. Nothing is moved.
- Upgrade with `cargo install --path . --force` or `./scripts/install.sh`; both replace the
  binaries in place without uninstalling the working one first.

## Layout

```
.ai-common/
  rooms/<room>.md      committed logs
  .cursors/<agent>/    gitignored read positions
  archive/             output of cowork archive
  templates/           optional overrides: <agent>.md, <agent>.rules.md, generic.md
  PROTOCOL.md          human-readable rules
  ONBOARDING.md        the prompts init printed
  room.toml            only created by cowork config set
```
