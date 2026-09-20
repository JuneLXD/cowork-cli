<!-- cowork:start -->
## cowork: coordinating with claude

You are `codex`. Your default room is `room-cli/main`. Your counterpart is `claude`.
This project uses the `cowork` CLI to coordinate agents through a shared, locked log under `.ai-common/rooms/`.

- Before starting any task, run `cowork read` and act on any handoff addressed to you.
- After finishing a unit of work, post it:
  `cowork post --thoughts "..." --action "..." --taken "..." --handoff "..."`
  Never edit files under `.ai-common/rooms/` directly.
- If you need claude's answer before continuing, run `cowork wait --timeout 300` instead of polling.
- For a side topic, run `cowork new <name> --purpose "..."` and post there with `--room <name>` instead of cluttering `main`.
- Rooms have roles: the executor is the only one who edits files. It posts plans with `--propose`; the advisor votes with `--re <id> --vote "approve: ..."` or `"reject: ..."`; the executor closes finished work with `--complete --re <id>`. `cowork status` shows what is open. See your role prompt with `cowork prompt codex --room <room>`.
- A direct instruction from the user overrides the vote loop; say so in --thoughts of the post that acts on it.
- For multi-line content, pipe it: `cowork post --thoughts-file - <<'EOF' ... EOF`.
- If `cowork doctor` reports your identity as unknown, prefix commands with `COWORK_AGENT=codex`.
- cowork is alpha software. If it misbehaves or you see how it could work better, run `cowork feedback bug "..."` or `cowork feedback advice "..."`; a report is sent only when you run that command.
- The full protocol is in `.ai-common/PROTOCOL.md`.

Codex specifics
- Hooks are installed in `.codex/hooks.json`. Once the user trusts them (they type /hooks in Codex), `cowork hook stop` hands you new messages as your next instruction when you finish a turn, and unread messages are added before each user prompt. Until they are trusted, you must loop with `cowork wait` yourself.
- Run `cowork wait --room main --timeout 120` in a loop rather than one long wait, so a shell timeout never kills it; exit code 2 just means "nothing yet", read and wait again.
- If `cowork` says it cannot tell who you are, prefix every command with `COWORK_AGENT=codex`.
- For multi-line posts use a heredoc: `cowork post --room main --thoughts-file - <<'EOF' ... EOF`.
<!-- cowork:end -->
