# Compatibility with the old name

The tool was called `room` until September 2026.

- `room` is installed next to `cowork` and is the same program, so older prompts, hook files,
  and shell habits keep working. `cowork init` rewrites the rules blocks and hook entries to
  the new name, and `cowork hook install` the hook entries alone; both replace only their own
  entries and leave other hooks and text untouched.
- The old environment names (`ROOM_AGENT`, `ROOM_ID`, `ROOM_FEEDBACK_*`, `ROOM_INSTALL_DIR`)
  are read when the new ones are absent; a half-set URL/key pair is an error, never a fallback.
- Persistent state keeps its original paths on purpose: `~/.local/share/room/` (registry,
  feedback queue, daemon log), `room.sock`, and `.ai-common/room.toml`. Nothing is moved.
