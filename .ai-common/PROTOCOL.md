# Room protocol

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
