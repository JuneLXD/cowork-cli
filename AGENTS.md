<!-- room-cli:start -->
## room-cli: coordinating with claude

You are `codex`. Your default room is `room-cli/main`. Your counterpart is `claude`.
This project uses the `room` CLI to coordinate agents through a shared, locked log under `.ai-common/rooms/`.

- Before starting any task, run `room read` and act on any handoff addressed to you.
- After finishing a unit of work, post it:
  `room post --thoughts "..." --action "..." --taken "..." --handoff "..."`
  Never edit files under `.ai-common/rooms/` directly.
- If you need claude's answer before continuing, run `room wait --timeout 300` instead of polling.
- For a side topic, run `room new <name> --purpose "..."` and post there with `--room <name>` instead of cluttering `main`.
- Rooms have roles: the executor is the only one who edits files, the advisor reviews and votes with `--vote "approve: ..."` or `--vote "reject: ..."`. See your role prompt with `room prompt codex --room <room>`.
- For multi-line content, pipe it: `room post --thoughts-file - <<'EOF' ... EOF`.
- If `room doctor` reports your identity as unknown, prefix commands with `ROOM_AGENT=codex`.
- The full protocol is in `.ai-common/PROTOCOL.md`.

Codex specifics
- Hooks are installed in `.codex/hooks.json`. Once the user trusts them (they type /hooks in Codex), `room hook stop` hands you new messages as your next instruction when you finish a turn, and unread messages are added before each user prompt. Until they are trusted, you must loop with `room wait` yourself.
- Run `room wait --room main --timeout 120` in a loop rather than one long wait, so a shell timeout never kills it; exit code 2 just means "nothing yet", read and wait again.
- If `room` says it cannot tell who you are, prefix every command with `ROOM_AGENT=codex`.
- For multi-line posts use a heredoc: `room post --room main --thoughts-file - <<'EOF' ... EOF`.
<!-- room-cli:end -->
