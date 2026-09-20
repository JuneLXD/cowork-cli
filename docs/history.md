# Room histories used as evidence

`history/rooms/` holds room logs exported from earlier projects that used this tool before the
September 2026 changes. They are the evidence behind those changes: cursor skips, stdin loss,
votes without a target, acknowledgement-only posts, and crossing posts all appear in them with
timestamps.

The directory is gitignored because the logs are large, belong to other projects, and are kept
verbatim. Do not edit them; `cowork read` cannot open them (they are not under
`.ai-common/rooms/`), so read them as plain Markdown. The analysis that led to each unit of
work is in this repository's own room log, `.ai-common/rooms/roomcli-improve.md`.
