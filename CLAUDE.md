<!-- room-cli:start -->
## room-cli: coordinating with codex

You are `claude`. Your default room is `room-cli/main`. Your counterpart is `codex`.
This project uses the `room` CLI to coordinate agents through a shared, locked log under `.ai-common/rooms/`.

- Before starting any task, run `room read` and act on any handoff addressed to you.
- After finishing a unit of work, post it:
  `room post --thoughts "..." --action "..." --taken "..." --handoff "..."`
  Never edit files under `.ai-common/rooms/` directly.
- If you need codex's answer before continuing, run `room wait --timeout 300` instead of polling.
- For a side topic, run `room new <name> --purpose "..."` and post there with `--room <name>` instead of cluttering `main`.
- Rooms have roles: the executor is the only one who edits files, the advisor reviews and votes with `--vote "approve: ..."` or `--vote "reject: ..."`. See your role prompt with `room prompt claude --room <room>`.
- For multi-line content, pipe it: `room post --thoughts-file - <<'EOF' ... EOF`.
- If `room doctor` reports your identity as unknown, prefix commands with `ROOM_AGENT=claude`.
- The full protocol is in `.ai-common/PROTOCOL.md`.

Claude Code specifics
- Hooks are installed in `.claude/settings.json`: when you finish a turn, `room hook stop` waits briefly and hands you any new message as your next instruction, and before each user prompt unread messages are added to your context. You do not need to poll.
- To keep working while you wait for `codex`, run `room wait --room main --timeout 600` with `run_in_background: true`; you get a task notification the moment `codex` posts, and background commands have no timeout.
- Foreground commands time out after 10 minutes, so never use a `--timeout` above 600 in the foreground.
- For multi-line posts use a heredoc: `room post --room main --thoughts-file - <<'EOF' ... EOF`.
<!-- room-cli:end -->
