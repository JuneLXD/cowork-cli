# Configuration

Optional keys in `.ai-common/room.toml`, set with `cowork config set <key> <value>`:

| Key | Default | Meaning |
|---|---|---|
| `agents` | `claude,codex` | two or three agents (`claude`, `codex`, `kimi`, or other names): `main`'s members when `cowork init` creates it, and the default for new rooms. Existing rooms keep their own members. |
| `name` | directory name | project name used in room addresses |
| `tail` | 40 | line budget for an agent's very first read; whole messages only, at least the latest |
| `wait_timeout` | 300 | default for `cowork wait` |
| `archive_keep` | 20 | messages kept by `cowork archive` |
| `hook_wait` | 120 | seconds the Stop hook waits for a message (at most 570 for kimi, under Kimi Code's 600-second hook limit) |
| `hook_max_continues` | 100 | Stop-hook continuations per session |
| `feedback_url`, `feedback_key` | built in | where `cowork feedback` sends reports (set both) |

Environment variables: `COWORK_AGENT`, `COWORK_ROOM` (default room), `COWORK_FEEDBACK_URL`
and `COWORK_FEEDBACK_KEY` (set together), `COWORK_FEEDBACK_TIMEOUT`, `COWORK_INSTALL_DIR`.
`KIMI_CODE_HOME` is read, as Kimi Code reads it, to find the config that
`cowork hook install --tool kimi` edits (default `~/.kimi-code/config.toml`).

Prompt templates can be overridden per project under `.ai-common/templates/`:
`executor.md`, `advisor.md`, `<agent>.md` (kickoff), `<agent>.rules.md`, `generic.md`.
