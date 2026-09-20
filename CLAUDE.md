<!-- cowork:start -->
## cowork: coordinating with codex

You are `claude`. Your default room is `room-cli/main`. Your counterpart is `codex`.
This project uses the `cowork` CLI to coordinate agents through a shared, locked log under `.ai-common/rooms/`.

- Before starting any task, run `cowork read` and act on any handoff addressed to you.
- After finishing a unit of work, post it:
  `cowork post --thoughts "..." --action "..." --taken "..." --handoff "..."`
  Never edit files under `.ai-common/rooms/` directly.
- If you need codex's answer before continuing, run `cowork wait --timeout 300` instead of polling.
- For a side topic, run `cowork new <name> --purpose "..."` and post there with `--room <name>` instead of cluttering `main`.
- Rooms have roles: the executor is the only one who edits files. It posts plans with `--propose`; the advisor votes with `--re <id> --vote "approve: ..."` or `"reject: ..."`; the executor closes finished work with `--complete --re <id>`. `cowork status` shows what is open. See your role prompt with `cowork prompt claude --room <room>`.
- A direct instruction from the user overrides the vote loop; say so in --thoughts of the post that acts on it.
- For multi-line content, pipe it: `cowork post --thoughts-file - <<'EOF' ... EOF`.
- If `cowork doctor` reports your identity as unknown, prefix commands with `COWORK_AGENT=claude`.
- cowork is alpha software. If it misbehaves or you see how it could work better, run `cowork feedback bug "..."` or `cowork feedback advice "..."`; a report is sent only when you run that command.
- The full protocol is in `.ai-common/PROTOCOL.md`.

Claude Code specifics
- Hooks are installed in `.claude/settings.json`: when you finish a turn, `cowork hook stop` waits briefly and hands you any new message as your next instruction, and before each user prompt unread messages are added to your context. You do not need to poll.
- To keep working while you wait for `codex`, run `cowork wait --room main --timeout 600` with `run_in_background: true`; you get a task notification the moment `codex` posts, and background commands have no timeout.
- Foreground commands time out after 10 minutes, so never use a `--timeout` above 600 in the foreground.
- For multi-line posts use a heredoc: `cowork post --room main --thoughts-file - <<'EOF' ... EOF`.
<!-- cowork:end -->
