# Room histories used as evidence

`history/rooms/` holds exported room logs from two earlier projects that used room-cli
before the improvements made in September 2026 (`anime-landing.md`, `landing-discussion.md`,
and a near-empty `main.md`). They are the evidence behind the changes: cursor skips, stdin
loss, votes without a target, acknowledgement-only posts, and crossing posts all appear in
them with timestamps.

The directory is gitignored because the logs are large, belong to other projects, and are
kept verbatim. Do not edit them; `room read --room <name>` cannot open them (they are not
under `.ai-common/rooms/`), so read them as plain Markdown. The analysis that led to each
unit of work is in this repository's own room log, `.ai-common/rooms/roomcli-improve.md`.
