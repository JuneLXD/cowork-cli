# Building from source

```sh
cargo build --release          # target/release/cowork and target/release/room
cargo test                     # end-to-end tests run the built binary in temporary projects
./scripts/export.sh            # self-contained bundle and tarball under export/ (gitignored)
```

The release binary links against glibc 2.30 or newer, which every Ubuntu from 20.04 provides.
A fully static build (`--target x86_64-unknown-linux-musl`) needs `musl-tools` on the build
machine for the TLS library.

`cowork feedback` needs an endpoint. `build.rs` reads `COWORK_FEEDBACK_URL` and
`COWORK_FEEDBACK_KEY` from the build environment, or `project_ID` and `publishable_key` from a
`.env` file next to `Cargo.toml`, and bakes them in. Only a publishable (insert-only) key is
accepted. A build without either source still works, and the command then explains how to set
an endpoint at runtime. The table schema and how to apply it are in [supabase/](../supabase/).

## Message format

```
### [claude] - 2026-09-20 10:00:00 UTC

- **Id:** 1
- **Proposal:** yes
- **Thoughts & Insight:** The nav wraps under 400 px
- **Proposed Action:** Add a media query in src/style.css
- **Action Taken / Code Changes:** None
- **Handoff / Questions for Counterpart:** None
```

Each message has an author, a timestamp, an id, optional `Re`, `Proposal`, `Completes` and
`Vote` markers, and four text fields. The file keeps every field; readers see only the fields
with content. Rooms have front matter with their purpose, participants, executor, and the
highest id ever allocated, so `cowork archive` can move old messages out without reusing ids.
