# Room protocol

This project coordinates AI coding agents through `cowork`, a CLI that appends to shared
Markdown logs under `.ai-common/rooms/` using kernel file locks. Every agent reads the
log before working and posts after each unit of work. Nobody edits the log by hand.

## Rules

1. Read before you work. `cowork read` shows what you have not seen yet. Act on any
   handoff addressed to you before starting new work.
2. Post after each unit of work. `cowork post` takes four fields:
   - thoughts: reasoning, critiques, observations
   - action: what you plan to do next
   - taken: what you just executed or modified
   - handoff: explicit questions or handoffs for the counterpart, or `None`
3. Handoffs are explicit. If you need the other agent to do or decide something, say so
   in the handoff field and name them.
4. Wait, do not poll. `cowork wait` blocks until the counterpart posts.
5. One topic per room. `cowork new <name>` creates a side room. Keep `main` for coordination.
6. Never edit files under `.ai-common/rooms/` directly. The CLI owns the format.
7. Every room has one executor and one or more advisors. The executor proposes each
   change with `--propose`, waits for a vote, and only then edits files. Advisors review
   and vote on the proposal id: `--re <id> --vote "approve: reason"` or `"reject: reason"`.
   A proposal is approved by one approve and blocked by any reject; only the latest vote
   per voter counts, and votes on your own proposal or without `--re` are informational.
   The executor closes finished work with `--complete --re <id>`; a revised plan is
   `--propose --re <id>`. `cowork status` lists every open proposal. Advisors never edit
   files. `cowork prompt <agent> --room <room>` prints the role prompt.
8. A direct instruction from the user overrides the vote loop; the post that acts on it
   says so.

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
| `cowork read` | unread messages (or `--tail N`, `--last N`, `--since TS`, `--json`) |
| `cowork post --thoughts ... --action ... --taken ... --handoff ...` | append a message (`--propose`, `--re <id>`, `--vote`, `--complete`) |
| `cowork wait --timeout 300` | block until someone else posts |
| `cowork new <name> --purpose "..."` | create a side room |
| `cowork list` / `cowork status` | overview of rooms and open handoffs |
| `cowork archive` | move old messages out of a long room |
| `cowork doctor` | check the setup |
| `cowork feedback bug\|advice "..."` | report a bug or suggestion to the cowork maintainers (alpha) |
