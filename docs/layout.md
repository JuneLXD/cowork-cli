# Repository layout

| Path | What it is |
|---|---|
| `src/` | The `cowork` binary. `commands.rs` is the command implementations, `room.rs` the log format, `state.rs` proposal state, `hooks.rs` the editor hooks, `feedback.rs` bug reports, `templates.rs` the prompts. |
| `tests/cli.rs` | End-to-end tests that run the built binary in isolated temporary projects. |
| `build.rs` | Bakes the feedback endpoint into the binary from `.env` or the environment. |
| `docs/` | `design.md` (the original design), `history.md` (what `history/` is), this file. |
| `scripts/install.sh` | Builds a release binary and installs it into `~/.local/bin`. |
| `supabase/` | The SQL for the feedback table and how to apply it. |
| `.ai-common/` | This repository's own rooms, prompts, and protocol, created by `cowork init`. Cursors under `.cursors/` are gitignored. |
| `.claude/`, `.codex/` | Editor hook configuration written by `cowork init`. Kimi Code's hooks live outside the repository, in its global `config.toml`, and only `cowork hook install --tool kimi` writes them. |
| `CLAUDE.md`, `AGENTS.md` | Optional rules blocks, controlled by `--agent-files` / `--no-agent-files` on `cowork init` and `cowork new`. |
| `history/` | Gitignored exported room logs from earlier projects (see `docs/history.md`). |
| `.env` | Gitignored credentials; only `project_ID` and `publishable_key` are read, at build time. |
