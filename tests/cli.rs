//! End-to-end checks against the built `room` binary in an isolated project.
//! Each test gets its own temp directory with a `.git` marker, and the registry
//! and daemon socket are redirected inside it so nothing touches the real ones.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        Self::with_agents(name, None)
    }

    fn with_agents(name: &str, agents: Option<&str>) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("room-cli-test-{}-{}-{}", std::process::id(), n, name));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("xdg-data")).unwrap();
        fs::create_dir_all(root.join("xdg-run")).unwrap();
        let p = Project { root };
        let mut args = vec!["init", "--quiet"];
        if let Some(a) = agents {
            args.extend(["--agents", a]);
        }
        let out = p.run("claude", &args, None);
        assert!(out.status.success(), "init failed: {}", String::from_utf8_lossy(&out.stderr));
        p
    }

    fn cmd(&self, agent: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_room"));
        c.current_dir(&self.root)
            .env("ROOM_AGENT", agent)
            .env("XDG_DATA_HOME", self.root.join("xdg-data"))
            .env("XDG_RUNTIME_DIR", self.root.join("xdg-run"))
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .env_remove("ROOM_ID");
        c
    }

    fn run(&self, agent: &str, args: &[&str], stdin: Option<&str>) -> Output {
        let mut c = self.cmd(agent);
        c.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().expect("spawn room");
        {
            let mut si = child.stdin.take().unwrap();
            if let Some(s) = stdin {
                si.write_all(s.as_bytes()).unwrap();
            }
        }
        child.wait_with_output().unwrap()
    }

    fn ok(&self, agent: &str, args: &[&str]) -> String {
        let out = self.run(agent, args, None);
        assert!(
            out.status.success(),
            "room {:?} as {agent} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn post(&self, agent: &str, thoughts: &str) {
        self.ok(agent, &["post", "--thoughts", thoughts]);
    }

    fn room_file(&self) -> PathBuf {
        self.root.join(".ai-common/rooms/main.md")
    }

    fn message_count(&self) -> usize {
        fs::read_to_string(self.room_file()).unwrap().matches("### [").count()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Sleep until the UTC second changes, so consecutive posts get distinct headers.
fn next_second() {
    let start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if now > start {
            return;
        }
    }
}

fn cursor(root: &Path, agent: &str) -> Option<String> {
    fs::read_to_string(root.join(".ai-common/.cursors").join(agent).join("main")).ok()
}

#[test]
fn first_post_then_reply_then_wait_sees_the_reply() {
    let p = Project::new("first-post");
    // claude has never read; its first action is a post.
    p.post("claude", "hello from claude");
    assert_eq!(cursor(&p.root, "claude").as_deref(), Some("\n"), "post seeds an empty cursor for a new reader");
    p.post("codex", "reply from codex");
    // wait must not initialise its cursor to the current tail and swallow the reply.
    let out = p.run("claude", &["wait", "--timeout", "1"], None);
    assert_eq!(out.status.code(), Some(0), "wait should return immediately with the reply");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("reply from codex"), "wait output: {text}");
    assert!(!text.contains("hello from claude"), "wait must not echo own post: {text}");
}

#[test]
fn post_does_not_skip_a_message_that_arrived_meanwhile() {
    let p = Project::new("interleave");
    p.post("codex", "c1");
    let first = p.ok("claude", &["read"]);
    assert!(first.contains("c1"));
    assert!(p.ok("claude", &["read"]).contains("(no unread messages in"));
    // codex posts while claude is composing; claude then posts without reading.
    // Cursors are keyed on the header line (agent + second), so two codex posts in
    // the same second would collide; stable message ids remove this in a later unit.
    next_second();
    p.post("codex", "c2 arrived during claude's long tool call");
    p.post("claude", "claude's reply written against c1 only");
    let unread = p.ok("claude", &["read"]);
    assert!(unread.contains("c2 arrived"), "post must not advance the cursor past c2: {unread}");
    assert!(!unread.contains("claude's reply"), "own post must not appear as unread: {unread}");
    assert!(p.ok("claude", &["read"]).contains("(no unread messages in"));
}

#[test]
fn double_stdin_is_rejected_and_appends_nothing() {
    let p = Project::new("double-stdin");
    let before = p.message_count();
    let out = p.run(
        "claude",
        &["post", "--thoughts-file", "-", "--action-file", "-"],
        Some("these thoughts would be recorded, the action silently lost\n"),
    );
    assert!(!out.status.success(), "second `-` must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--thoughts-file, --action-file"), "error names the flags: {err}");
    assert_eq!(p.message_count(), before, "nothing appended");
}

#[test]
fn single_stdin_field_still_works() {
    let p = Project::new("single-stdin");
    let out = p.run(
        "claude",
        &["post", "--thoughts-file", "-", "--action", "inline action"],
        Some("line one\nline two\n"),
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = p.ok("codex", &["read", "--last", "1"]);
    assert!(text.contains("line one\n  line two"), "multi-line thoughts kept: {text}");
    assert!(text.contains("**Proposed Action:** inline action"), "{text}");
}

#[test]
fn first_read_hides_own_posts_but_agent_filter_shows_them() {
    let p = Project::new("first-read");
    // codex has no cursor yet; the first read falls back to the recent tail.
    p.post("claude", "from claude");
    p.post("codex", "from codex, posted without ever reading");
    // Remove codex's cursor to simulate the pure first-read path.
    fs::remove_file(p.root.join(".ai-common/.cursors/codex/main")).unwrap();
    let text = p.ok("codex", &["read"]);
    assert!(text.contains("from claude"), "{text}");
    assert!(!text.contains("posted without ever reading"), "own post hidden on first read: {text}");
    // Explicit inspection of one's own posts is still possible.
    let mine = p.ok("codex", &["read", "--last", "5", "--agent", "codex"]);
    assert!(mine.contains("posted without ever reading"), "{mine}");
    // And --last is untouched by the own-post filter.
    let last = p.ok("codex", &["read", "--last", "5"]);
    assert!(last.contains("posted without ever reading") && last.contains("from claude"), "{last}");
}

#[test]
fn fresh_reader_gets_a_long_review_that_exceeds_the_tail_budget() {
    let p = Project::new("long-first-read");
    let review: String = (1..=70).map(|i| format!("review line {i}\n")).collect();
    let out = p.run(
        "codex",
        &["post", "--thoughts-file", "-", "--vote", "reject: needs the long fix"],
        Some(&review),
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // claude has no cursor at all: the first read must still deliver the whole message.
    let json = p.ok("claude", &["read", "--no-advance", "--json"]);
    assert!(json.contains("review line 70") && json.contains("reject: needs the long fix"), "{json}");
    let md = p.ok("claude", &["read", "--no-advance"]);
    assert!(md.contains("review line 1\n") && md.contains("**Vote:** reject"), "{md}");
    assert!(cursor(&p.root, "claude").is_none(), "--no-advance leaves no cursor");
    let normal = p.ok("claude", &["read"]);
    assert!(normal.contains("review line 70"), "{normal}");
    assert!(p.ok("claude", &["read"]).contains("(no unread messages in"));
}

#[test]
fn fresh_reader_tail_keeps_whole_messages_within_budget() {
    let p = Project::new("tail-budget");
    for i in 1..=12 {
        p.post("codex", &format!("short post {i}"));
    }
    let text = p.ok("claude", &["read", "--no-advance"]);
    // Each short post renders to 7 lines, so a 40-line budget holds the last 5.
    assert!(text.contains("short post 12") && text.contains("short post 8"), "{text}");
    assert!(!text.contains("short post 7"), "older posts beyond the budget are left out: {text}");
    assert!(!text.contains("### [codex]\n"), "no partial message blocks: {text}");
}

// ---------- unit 2a: message ids ----------

fn room_text(p: &Project) -> String {
    fs::read_to_string(p.room_file()).unwrap()
}

fn append_raw(p: &Project, text: &str) {
    let mut f = fs::OpenOptions::new().append(true).open(p.room_file()).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

fn write_cursor(p: &Project, agent: &str, token: &str) {
    let dir = p.root.join(".ai-common/.cursors").join(agent);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("main"), format!("{token}\n")).unwrap();
}

#[test]
fn ids_are_contiguous_across_a_same_second_burst() {
    let p = Project::new("ids-burst");
    for i in 1..=5 {
        let out = p.ok("codex", &["post", "--thoughts", &format!("burst {i}")]);
        assert!(out.starts_with(&format!("posted #{i} to")), "{out}");
    }
    let text = room_text(&p);
    for i in 1..=5 {
        assert!(text.contains(&format!("- **Id:** {i}\n")), "id {i} missing:\n{text}");
    }
    let json = p.ok("claude", &["read", "--last", "1", "--json"]);
    assert!(json.contains("\"id\": 5"), "{json}");
    // Same-second posts are distinct to a reader: nothing is skipped or repeated.
    let unread = p.ok("claude", &["read"]);
    assert_eq!(unread.matches("### [codex]").count(), 5, "{unread}");
}

#[test]
fn archive_keep_zero_then_post_continues_numbering() {
    let p = Project::new("archive-ids");
    for i in 1..=3 {
        p.post("codex", &format!("m{i}"));
    }
    p.ok("codex", &["archive", "--keep", "0"]);
    let text = room_text(&p);
    assert!(text.contains("last_id: 3\n"), "front matter keeps the high-water mark:\n{text}");
    assert!(!text.contains("### ["), "room is empty after archive:\n{text}");
    let out = p.ok("codex", &["post", "--thoughts", "after archive"]);
    assert!(out.starts_with("posted #4 to"), "{out}");
    p.ok("codex", &["archive", "--keep", "0"]);
    assert!(room_text(&p).contains("last_id: 4\n"));
    let archive_dir = p.root.join(".ai-common/archive");
    let archived = fs::read_dir(archive_dir).unwrap().map(|e| fs::read_to_string(e.unwrap().path()).unwrap()).collect::<String>();
    assert!(archived.contains("- **Id:** 1\n") && archived.contains("- **Id:** 4\n"), "archived messages keep their ids:\n{archived}");
}

#[test]
fn legacy_header_cursor_skips_only_the_legacy_message() {
    let p = Project::new("legacy-cursor");
    let header = "### [codex] - 2026-01-01 00:00:00 UTC";
    append_raw(&p, &format!("{header}\n\n- **Thoughts & Insight:** legacy message\n- **Proposed Action:** None\n- **Action Taken / Code Changes:** None\n- **Handoff / Questions for Counterpart:** None\n\n"));
    write_cursor(&p, "claude", header);
    p.post("codex", "new message with an id");
    let unread = p.ok("claude", &["read"]);
    assert!(!unread.contains("legacy message"), "the legacy message was read already: {unread}");
    assert!(unread.contains("new message with an id"), "{unread}");
    // After that read the cursor is an id token.
    assert!(cursor(&p.root, "claude").unwrap().starts_with("id:"));
}

#[test]
fn legacy_header_cursor_replays_an_id_message_with_the_same_header() {
    let p = Project::new("legacy-replay");
    let header = "### [codex] - 2026-01-01 00:00:00 UTC";
    // The legacy message this cursor pointed at was archived; a newer id-bearing post
    // shares its header. Replaying it is the safe outcome.
    append_raw(&p, &format!("{header}\n\n- **Id:** 1\n- **Thoughts & Insight:** replay me\n- **Proposed Action:** None\n- **Action Taken / Code Changes:** None\n- **Handoff / Questions for Counterpart:** None\n\n"));
    write_cursor(&p, "claude", header);
    let unread = p.ok("claude", &["read"]);
    assert!(unread.contains("replay me"), "{unread}");
}

#[test]
fn id_cursor_survives_archive() {
    let p = Project::new("id-cursor-archive");
    p.post("codex", "one");
    p.post("codex", "two");
    p.ok("claude", &["read"]);
    assert_eq!(cursor(&p.root, "claude").as_deref(), Some("id:2\n"));
    p.ok("codex", &["archive", "--keep", "1"]);
    p.post("codex", "three");
    let unread = p.ok("claude", &["read"]);
    assert!(unread.contains("three") && !unread.contains("two"), "{unread}");
}

#[test]
fn invalid_reply_targets_append_nothing() {
    let p = Project::new("bad-re");
    p.post("codex", "proposal");
    let before = p.message_count();
    let out = p.run("claude", &["post", "--thoughts", "x", "--re", "99"], None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("newest id is 1"));
    let out = p.run("claude", &["post", "--thoughts", "x", "--re", "0"], None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("1 or more"));
    assert_eq!(p.message_count(), before);
    let ok = p.ok("claude", &["post", "--thoughts", "reply", "--re", "1"]);
    assert!(ok.starts_with("posted #2"));
    assert!(room_text(&p).contains("- **Re:** #1\n"));
    let json = p.ok("codex", &["read", "--json"]);
    assert!(json.contains("\"re\": 1"), "{json}");
}

#[test]
fn votes_are_validated_and_normalized() {
    let p = Project::new("votes");
    p.post("codex", "proposal");
    for bad in ["approved -", "yes", "approve", "approve:", "maybe: later"] {
        let out = p.run("claude", &["post", "--thoughts", "x", "--vote", bad, "--re", "1"], None);
        assert!(!out.status.success(), "vote `{bad}` should be rejected");
    }
    assert_eq!(p.message_count(), 1);
    let out = p.run("claude", &["post", "--thoughts", "x", "--vote", "Approve:  looks right", "--re", "1"], None);
    assert!(out.status.success());
    assert!(room_text(&p).contains("- **Vote:** approve: looks right\n"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("not linked"));
    let out = p.run("claude", &["post", "--thoughts", "x", "--vote", "reject: unlinked"], None);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not linked"), "unlinked vote warns");
}

#[test]
fn hook_prompt_delivers_a_same_second_follow_up() {
    let p = Project::new("hook-same-second");
    let cwd = format!("{{\"cwd\": {:?}}}", p.root.to_string_lossy());
    p.post("codex", "first");
    let out = p.run("claude", &["hook", "prompt", "--agent", "claude"], Some(&cwd));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("first"), "{text}");
    p.post("codex", "second, same second as the first");
    let out = p.run("claude", &["hook", "prompt", "--agent", "claude"], Some(&cwd));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("second, same second") && !text.contains("\"first"), "{text}");
}

#[test]
fn concurrent_posts_get_distinct_contiguous_ids() {
    let p = Project::new("concurrent");
    let mut children = Vec::new();
    for i in 0..16 {
        let mut c = p.cmd("codex");
        c.args(["post", "--thoughts", &format!("parallel {i}")]).stdout(Stdio::piped()).stderr(Stdio::piped());
        children.push(c.spawn().unwrap());
    }
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let text = room_text(&p);
    let mut ids: Vec<u64> = text.lines().filter_map(|l| l.strip_prefix("- **Id:** ")).map(|n| n.parse().unwrap()).collect();
    ids.sort();
    assert_eq!(ids, (1..=16).collect::<Vec<u64>>(), "{text}");
}

// ---------- feedback ----------

use std::io::{BufRead, BufReader, Read as _};
use std::net::TcpListener;

/// A one-shot HTTP server that answers every request with the same status and body
/// and records the request bodies it saw. `delay_ms` sleeps before answering.
fn mock_server(status: u16, body: &'static str, delay_ms: u64, n: usize) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for _ in 0..n {
            let Ok((stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream);
            let mut len = 0usize;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).ok();
            seen2.lock().unwrap().push(String::from_utf8_lossy(&buf).to_string());
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = reader.get_mut().write_all(resp.as_bytes());
        }
    });
    (url, seen)
}

fn feedback(p: &Project, url: &str, args: &[&str], stdin: Option<&str>) -> Output {
    let mut c = p.cmd("codex");
    c.env("ROOM_FEEDBACK_URL", url)
        .env("ROOM_FEEDBACK_KEY", "sb_publishable_test")
        .env("ROOM_FEEDBACK_TIMEOUT", "1")
        .args(["feedback"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    {
        let mut si = child.stdin.take().unwrap();
        if let Some(s) = stdin {
            si.write_all(s.as_bytes()).unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

fn queue_files(p: &Project) -> Vec<PathBuf> {
    let dir = p.root.join("xdg-data/room/feedback-queue");
    let mut v: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    v.sort();
    v
}

#[test]
fn feedback_sends_the_minimal_payload_by_default() {
    let p = Project::new("fb-send");
    let (url, seen) = mock_server(201, "", 0, 1);
    let out = feedback(&p, &url, &["bug", "it broke"], None);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("sent bug report "));
    let body = seen.lock().unwrap()[0].clone();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["kind"], "bug");
    assert_eq!(v["message"], "it broke");
    assert_eq!(v["reporter"], "codex");
    assert!(v["version"].as_str().unwrap().starts_with(char::is_numeric));
    assert!(v.get("project").is_none() && v.get("room").is_none() && v.get("context").is_none(), "opt-in fields absent: {body}");
    assert!(queue_files(&p).is_empty());
}

#[test]
fn feedback_opt_in_fields_and_stdin() {
    let p = Project::new("fb-optin");
    let (url, seen) = mock_server(201, "", 0, 1);
    let out = feedback(&p, &url, &["advice", "--file", "-", "--room", "main", "--context", "ran: room wait"], Some("line a\nline b\n"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
    assert_eq!(v["kind"], "advice");
    assert_eq!(v["message"], "line a\nline b");
    assert_eq!(v["room"], "main");
    assert!(v["project"].as_str().unwrap().contains("fb-optin"));
    assert_eq!(v["context"], "ran: room wait");
}

#[test]
fn feedback_double_stdin_and_oversize_are_rejected_before_sending() {
    let p = Project::new("fb-reject");
    let (url, seen) = mock_server(201, "", 0, 1);
    let out = feedback(&p, &url, &["bug", "--file", "-", "--context-file", "-"], Some("x"));
    assert!(!out.status.success());
    let big = "x".repeat(8001);
    let out = feedback(&p, &url, &["bug", &big], None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("8000"));
    assert!(seen.lock().unwrap().is_empty(), "nothing was sent");
    assert!(queue_files(&p).is_empty(), "nothing was queued");
}

#[test]
fn feedback_duplicate_primary_key_counts_as_sent() {
    let p = Project::new("fb-dup");
    let (url, _) = mock_server(409, r#"{"code":"23505","message":"duplicate key value violates unique constraint \"feedback_pkey\""}"#, 0, 1);
    let out = feedback(&p, &url, &["bug", "again"], None);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("already stored"));
    assert!(queue_files(&p).is_empty());
}

#[test]
fn feedback_unrelated_409_is_queued_not_claimed_sent() {
    let p = Project::new("fb-409");
    let (url, _) = mock_server(409, r#"{"code":"23503","message":"foreign key"}"#, 0, 1);
    let out = feedback(&p, &url, &["bug", "hm"], None);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("queued bug report "));
    assert_eq!(queue_files(&p).len(), 1);
}

#[test]
fn feedback_timeout_queues_and_retry_resends_to_the_original_endpoint() {
    let p = Project::new("fb-timeout");
    let (slow, _) = mock_server(201, "", 2500, 1);
    let out = feedback(&p, &slow, &["bug", "slow server"], None);
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stdout));
    let files = queue_files(&p);
    assert_eq!(files.len(), 1);
    let q: serde_json::Value = serde_json::from_str(&fs::read_to_string(&files[0]).unwrap()).unwrap();
    assert_eq!(q["url"], slow.as_str());
    let id = q["report"]["id"].as_str().unwrap().to_string();

    // A retry that still fails keeps the report.
    let (down, _) = mock_server(500, "", 0, 1);
    let out = feedback(&p, &down, &["retry"], None);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(queue_files(&p).len(), 1);
    let listed = String::from_utf8_lossy(&feedback(&p, &down, &["list"], None).stdout).to_string();
    assert!(listed.contains(&id) && listed.contains(&slow), "{listed}");

    // Retry goes to the endpoint recorded in the queue, not the current one.
    // Rewrite the queued url to a fresh mock to prove which one is used.
    let (good, seen) = mock_server(201, "", 0, 1);
    let mut q = q;
    q["url"] = serde_json::Value::String(good.clone());
    fs::write(&files[0], q.to_string()).unwrap();
    let (other, other_seen) = mock_server(201, "", 0, 1);
    let out = feedback(&p, &other, &["retry"], None);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(queue_files(&p).is_empty());
    assert_eq!(seen.lock().unwrap().len(), 1, "sent to the recorded endpoint");
    assert!(other_seen.lock().unwrap().is_empty(), "current endpoint untouched");
    let v: serde_json::Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
    assert_eq!(v["id"], id.as_str(), "same id on retry");
}

#[test]
fn binary_contains_no_server_side_credentials() {
    let bin = fs::read(env!("CARGO_BIN_EXE_room")).unwrap();
    let text = String::from_utf8_lossy(&bin);
    assert!(!text.contains("sb_secret_"), "secret key must never be embedded");
    // If a .env exists next to Cargo.toml, its server-side values must be absent too.
    if let Ok(env) = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")) {
        for line in env.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim();
                if ["service_role", "secret_key"].contains(&k.trim()) && v.len() > 20 {
                    assert!(!text.contains(v), "{k} value found in the binary");
                }
            }
        }
    }
}

#[test]
fn feedback_duplicate_needs_code_and_constraint() {
    // Same SQLSTATE on another constraint: not this report, so it is queued.
    let p = Project::new("fb-dup-other");
    let (url, _) = mock_server(409, r#"{"code":"23505","message":"duplicate key value violates unique constraint \"other_idx\""}"#, 0, 1);
    let out = feedback(&p, &url, &["bug", "x"], None);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(queue_files(&p).len(), 1);
    // The code only as loose text in an unstructured body: queued too.
    let p2 = Project::new("fb-dup-text");
    let (url, _) = mock_server(409, "error 23505 feedback_pkey", 0, 1);
    let out = feedback(&p2, &url, &["bug", "x"], None);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(queue_files(&p2).len(), 1);
}

#[test]
fn feedback_partial_or_invalid_runtime_endpoint_is_an_error_not_a_fallback() {
    let p = Project::new("fb-endpoint");
    let (url, seen) = mock_server(201, "", 0, 1);
    // Only the url set in the environment.
    let out = p.cmd("codex").env("ROOM_FEEDBACK_URL", &url).env_remove("ROOM_FEEDBACK_KEY").args(["feedback", "bug", "x"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("incomplete"));
    // Only the key set in room.toml.
    p.ok("codex", &["config", "set", "feedback_key", "sb_publishable_abc"]);
    let out = p.cmd("codex").env_remove("ROOM_FEEDBACK_URL").env_remove("ROOM_FEEDBACK_KEY").args(["feedback", "bug", "x"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("incomplete"));
    // A server-side key is refused at runtime as well as at build time.
    p.ok("codex", &["config", "set", "feedback_url", &url]);
    p.ok("codex", &["config", "set", "feedback_key", "sb_secret_nope"]);
    let out = p.cmd("codex").env_remove("ROOM_FEEDBACK_URL").env_remove("ROOM_FEEDBACK_KEY").args(["feedback", "bug", "x"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a publishable key"));
    assert!(seen.lock().unwrap().is_empty(), "nothing was sent in any of these cases");
    assert!(queue_files(&p).is_empty(), "nothing was queued either");
    // A complete, valid room.toml pair works.
    p.ok("codex", &["config", "set", "feedback_key", "sb_publishable_abc"]);
    let out = p.cmd("codex").env_remove("ROOM_FEEDBACK_URL").env_remove("ROOM_FEEDBACK_KEY").args(["feedback", "bug", "x"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn feedback_reports_malformed_queue_files() {
    let p = Project::new("fb-badqueue");
    let dir = p.root.join("xdg-data/room/feedback-queue");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("broken.json"), "{not json").unwrap();
    let (url, _) = mock_server(201, "", 0, 1);
    let out = feedback(&p, &url, &["list"], None);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unreadable file"));
    assert!(dir.join("broken.json").exists(), "left untouched");
    let out = feedback(&p, &url, &["retry"], None);
    assert_eq!(out.status.code(), Some(3));
    // A valid queued report next to the broken file: listed, and still exit 3.
    let (down, _) = mock_server(500, "", 0, 1);
    assert_eq!(feedback(&p, &down, &["bug", "queued one"], None).status.code(), Some(3));
    let out = feedback(&p, &url, &["list"], None);
    assert_eq!(out.status.code(), Some(3));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("queued one") && text.contains("1 queued"), "{text}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("unreadable file"));
}

// ---------- unit 2b: proposals ----------

fn status_json(p: &Project) -> serde_json::Value {
    let text = p.ok("claude", &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v[0].clone()
}

fn proposal<'a>(s: &'a serde_json::Value, id: u64) -> &'a serde_json::Value {
    s["proposals"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap()
}

fn post_args(p: &Project, agent: &str, args: &[&str]) -> Output {
    let mut all = vec!["post"];
    all.extend_from_slice(args);
    p.run(agent, &all, None)
}

#[test]
fn proposal_state_table() {
    // main has no executor in its front matter: falls back to the first participant (claude).
    let p = Project::new("prop-states");
    let out = p.ok("claude", &["post", "--propose", "--thoughts", "why", "--action", "change a.rs"]);
    assert!(out.contains("posted #1") && out.contains("(proposal; codex votes with: room post --room main --re 1"), "{out}");
    let s = status_json(&p);
    assert_eq!(s["executor"], "claude");
    assert_eq!(proposal(&s, 1)["state"], "pending");
    assert_eq!(proposal(&s, 1)["waiting_for"], serde_json::json!(["codex"]));
    assert_eq!(proposal(&s, 1)["summary"], "change a.rs");

    let out = p.ok("codex", &["post", "--thoughts", "ok", "--re", "1", "--vote", "abstain: no opinion"]);
    assert!(out.contains("(abstain on #1)"), "{out}");
    assert_eq!(proposal(&status_json(&p), 1)["state"], "abstained");

    // Latest vote per voter wins.
    p.ok("codex", &["post", "--thoughts", "hm", "--re", "1", "--vote", "reject: needs tests"]);
    assert_eq!(proposal(&status_json(&p), 1)["state"], "rejected");
    p.ok("codex", &["post", "--thoughts", "fine", "--re", "1", "--vote", "approve: tests added"]);
    let s = status_json(&p);
    assert_eq!(proposal(&s, 1)["state"], "approved");
    assert_eq!(proposal(&s, 1)["votes"].as_array().unwrap().len(), 1, "one counted voter");
    assert_eq!(proposal(&s, 1)["waiting_for"], serde_json::json!([]));

    // Partial progress does not close it.
    p.ok("claude", &["post", "--thoughts", "half done", "--re", "1", "--taken", "edited a.rs"]);
    assert_eq!(proposal(&status_json(&p), 1)["state"], "approved");
    let text = p.ok("claude", &["status"]);
    assert!(text.contains("approved, not completed:") && text.contains("#1 claude"), "{text}");

    // Explicit completion closes it.
    let out = p.ok("claude", &["post", "--thoughts", "done", "--re", "1", "--complete", "--taken", "all edits in"]);
    assert!(out.contains("(completes #1)"), "{out}");
    let s = status_json(&p);
    assert_eq!(proposal(&s, 1)["state"], "completed");
    assert_eq!(proposal(&s, 1)["completed_by"], 6);
    let text = p.ok("claude", &["status"]);
    assert!(text.contains("recent: #1 completed by #6"), "{text}");

    // Supersede: a revised proposal replying to your own.
    p.ok("claude", &["post", "--propose", "--thoughts", "plan b", "--action", "change b.rs"]);
    p.ok("claude", &["post", "--propose", "--re", "7", "--thoughts", "plan b2", "--action", "change b.rs differently"]);
    let s = status_json(&p);
    assert_eq!(proposal(&s, 7)["state"], "superseded");
    assert_eq!(proposal(&s, 7)["superseded_by"], 8);
    assert_eq!(proposal(&s, 8)["state"], "pending");
}

#[test]
fn informational_votes_never_change_state() {
    let p = Project::new("prop-info");
    p.ok("claude", &["post", "--propose", "--thoughts", "t", "--action", "x"]);
    // Self-vote.
    let out = post_args(&p, "claude", &["--thoughts", "me", "--re", "1", "--vote", "approve: I like it"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("your own proposal"));
    // Unknown author.
    let out = post_args(&p, "stranger", &["--thoughts", "hi", "--re", "1", "--vote", "approve: sure"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a participant"));
    // Unlinked vote and a vote on a non-proposal.
    post_args(&p, "codex", &["--thoughts", "u", "--vote", "approve: unlinked"]);
    let out = post_args(&p, "codex", &["--thoughts", "n", "--re", "2", "--vote", "approve: on a reply"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a proposal"));
    let s = status_json(&p);
    assert_eq!(proposal(&s, 1)["state"], "pending");
    assert_eq!(s["informational_votes"], 4);
    let text = p.ok("claude", &["status"]);
    assert!(text.contains("informational votes") && text.contains("awaiting votes:"), "{text}");
}

#[test]
fn multi_advisor_waiting_list_and_backlog() {
    let p = Project::with_agents("prop-multi", Some("claude,codex,gemini"));
    p.ok("claude", &["post", "--propose", "--thoughts", "t", "--action", "first"]);
    p.ok("claude", &["post", "--propose", "--thoughts", "t", "--action", "second"]);
    p.ok("codex", &["post", "--thoughts", "ok", "--re", "1", "--vote", "approve: fine"]);
    let s = status_json(&p);
    assert_eq!(proposal(&s, 1)["state"], "approved", "approved by one");
    assert_eq!(proposal(&s, 1)["waiting_for"], serde_json::json!(["gemini"]));
    assert_eq!(proposal(&s, 2)["waiting_for"], serde_json::json!(["codex", "gemini"]));
    p.ok("gemini", &["post", "--thoughts", "no", "--re", "1", "--vote", "reject: blocked"]);
    assert_eq!(proposal(&status_json(&p), 1)["state"], "rejected", "blocked by any reject");
    let text = p.ok("claude", &["status"]);
    assert!(text.contains("rejected, not revised:") && text.contains("awaiting votes:"), "both listed: {text}");
    assert!(text.contains("waiting for: codex, gemini"), "{text}");
}

#[test]
fn executor_completes_an_advisor_proposal_and_bad_completes_fail() {
    let p = Project::with_agents("prop-complete", Some("claude,codex,gemini"));
    p.ok("codex", &["post", "--propose", "--thoughts", "idea", "--action", "advisor idea"]);
    p.ok("claude", &["post", "--thoughts", "ok", "--re", "1", "--vote", "approve: do it"]);
    let before = p.message_count();
    // A third party cannot complete it.
    let out = post_args(&p, "gemini", &["--thoughts", "done?", "--re", "1", "--complete"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("only codex (its author) or claude (the executor)"));
    // --complete needs --re, and --re must be a proposal.
    let out = post_args(&p, "claude", &["--thoughts", "x", "--complete"]);
    assert!(!out.status.success());
    let out = post_args(&p, "claude", &["--thoughts", "x", "--re", "2", "--complete"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not a proposal"));
    // --propose and --complete together are refused.
    let out = post_args(&p, "claude", &["--thoughts", "x", "--re", "1", "--complete", "--propose"]);
    assert!(!out.status.success());
    assert_eq!(p.message_count(), before, "nothing appended by the failures");
    // The executor may complete the advisor's proposal.
    let out = p.ok("claude", &["post", "--thoughts", "shipped", "--re", "1", "--complete", "--taken", "done"]);
    assert!(out.contains("(completes #1)"));
    assert_eq!(proposal(&status_json(&p), 1)["state"], "completed");
    // Completing it twice is refused.
    let out = post_args(&p, "claude", &["--thoughts", "again", "--re", "1", "--complete"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already completed"));
}

#[test]
fn archive_keeps_open_proposal_threads() {
    let p = Project::new("prop-archive");
    p.post("codex", "chatter 1");
    p.ok("claude", &["post", "--propose", "--thoughts", "t", "--action", "open plan"]);   // #2
    p.ok("codex", &["post", "--thoughts", "ok", "--re", "2", "--vote", "abstain: later"]); // #3
    p.post("codex", "chatter 4");
    p.post("codex", "chatter 5");
    let out = p.ok("codex", &["archive", "--keep", "1"]);
    assert!(out.contains("archived 2 messages") && out.contains("kept 2 older message(s)"), "{out}");
    let text = room_text(&p);
    assert!(text.contains("open plan") && text.contains("abstain: later"), "thread kept:\n{text}");
    assert!(text.contains("chatter 5"), "the newest message always stays:\n{text}");
    assert!(!text.contains("chatter 1") && !text.contains("chatter 4"), "others archived:\n{text}");
    assert!(text.contains("last_id: 5"));
    let s = status_json(&p);
    assert_eq!(proposal(&s, 2)["state"], "abstained");
    assert_eq!(proposal(&s, 2)["votes"][0]["id"], 3);
    // Once closed, the thread can be archived.
    p.ok("claude", &["post", "--thoughts", "closing", "--re", "2", "--complete"]);
    let out = p.ok("codex", &["archive", "--keep", "0"]);
    assert!(out.contains("archived 4 messages"), "{out}");
    assert!(!room_text(&p).contains("### ["));
}

#[test]
fn roster_comes_from_the_room_not_the_changed_project_config() {
    let p = Project::new("prop-roster");
    p.ok("claude", &["post", "--propose", "--thoughts", "t", "--action", "x"]);
    // The project config changes after the room exists; the room's own roster rules.
    p.ok("claude", &["config", "set", "agents", "gemini,claude,codex"]);
    let s = status_json(&p);
    assert_eq!(s["executor"], "claude");
    let before = p.message_count();
    let out = post_args(&p, "gemini", &["--thoughts", "done", "--re", "1", "--complete"]);
    assert!(!out.status.success(), "gemini is neither author nor executor of this room");
    assert!(String::from_utf8_lossy(&out.stderr).contains("only claude (its author) or claude (the executor)"));
    let out = post_args(&p, "gemini", &["--re", "1", "--vote", "approve: outsider"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a participant"));
    assert_eq!(proposal(&status_json(&p), 1)["state"], "pending");
    assert_eq!(p.message_count(), before + 1);
}

#[test]
fn vote_only_post_matches_the_printed_hint() {
    let p = Project::new("vote-only");
    let out = p.ok("claude", &["post", "--propose", "--thoughts", "t", "--action", "x"]);
    // Extract the hinted command and run it as codex, verbatim apart from the reason.
    let hint = out.split("votes with: ").nth(1).unwrap().split(" [--thoughts").next().unwrap().to_string();
    assert_eq!(hint, "room post --room main --re 1 --vote \"approve: reason\"");
    let out = post_args(&p, "codex", &["--room", "main", "--re", "1", "--vote", "approve: reason"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("(approve on #1)"));
    assert_eq!(proposal(&status_json(&p), 1)["state"], "approved");
    // Nothing substantive at all is still refused.
    let out = post_args(&p, "codex", &["--action", "only a plan"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--thoughts"));
}

// ---------- unit 3: display, crossing note, --brief ----------

#[test]
fn readers_see_only_fields_with_content_but_the_log_keeps_all() {
    let p = Project::new("compact");
    p.ok("claude", &["post", "--propose", "--thoughts", "why", "--action", "the plan"]);
    p.ok("codex", &["post", "--re", "1", "--vote", "approve: fine"]);
    let shown = p.ok("claude", &["read"]);
    assert!(shown.contains("- **Vote:** approve: fine") && shown.contains("- **Re:** #1"), "{shown}");
    assert!(!shown.contains("None"), "no None lines for readers:\n{shown}");
    let file = room_text(&p);
    assert!(file.contains("- **Thoughts & Insight:** None") && file.contains("- **Handoff / Questions for Counterpart:** None"), "log keeps every field:\n{file}");
    let json = p.ok("claude", &["read", "--last", "1", "--json"]);
    assert!(json.contains("\"thoughts\": \"None\""), "{json}");
    // A multi-line field whose first line is "None" is kept whole; a lone "none" is not.
    p.ok("codex", &["post", "--thoughts", "None\nThere is a concrete exception on line 2.", "--action", "none"]);
    let shown = p.ok("claude", &["read", "--last", "1"]);
    assert!(shown.contains("- **Thoughts & Insight:** None\n  There is a concrete exception on line 2."), "{shown}");
    assert!(!shown.contains("Proposed Action"), "lowercase none is still omitted: {shown}");
    p.ok("claude", &["read"]);
    // wait and the hook use the same compact rendering.
    p.ok("codex", &["post", "--thoughts", "just thoughts"]);
    let out = p.run("claude", &["wait", "--timeout", "1"], None);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("just thoughts") && !text.contains("None"), "{text}");
    let cwd = format!("{{\"cwd\": {:?}}}", p.root.to_string_lossy());
    p.ok("codex", &["post", "--thoughts", "for the hook"]);
    let out = p.run("claude", &["hook", "prompt", "--agent", "claude"], Some(&cwd));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("for the hook") && !text.contains("Proposed Action"), "{text}");
}

#[test]
fn crossing_note_appears_only_when_others_unread_exist() {
    let p = Project::new("crossing");
    p.post("codex", "c1");
    p.ok("claude", &["read"]);
    // Read-cleared cursor: no note.
    let out = post_args(&p, "claude", &["--thoughts", "reply"]);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("precede this post"));
    // Own posts only since the last read: no note.
    let out = post_args(&p, "claude", &["--thoughts", "another of mine"]);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("precede this post"));
    // Two unread from codex: note names them and their ids.
    p.post("codex", "c4");
    p.post("codex", "c5");
    let out = post_args(&p, "claude", &["--thoughts", "posted blind"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("2 unread message(s) from codex (#4, #5) precede this post"), "{err}");
    // The note left them unread.
    let unread = p.ok("claude", &["read"]);
    assert!(unread.contains("c4") && unread.contains("c5"), "{unread}");
    let out = post_args(&p, "claude", &["--thoughts", "after reading"]);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("precede this post"));
}

#[test]
fn brief_view_is_one_line_per_message_with_markers() {
    let p = Project::new("brief");
    p.ok("claude", &["post", "--propose", "--thoughts", "why", "--action", "a plan with a rather long first line that keeps going and going well past eighty characters"]);
    p.ok("codex", &["post", "--re", "1", "--vote", "reject: no"]);
    p.ok("claude", &["post", "--thoughts", "me too", "--re", "1", "--vote", "approve: self"]);
    p.ok("claude", &["post", "--re", "1", "--taken", "partial", "--complete"]);
    p.ok("codex", &["post", "--thoughts", "just a note"]);
    let brief = p.ok("claude", &["read", "--last", "5", "--brief"]);
    let lines: Vec<&str> = brief.lines().collect();
    assert_eq!(lines.len(), 5, "{brief}");
    assert!(lines[0].contains("#1") && lines[0].contains("[proposal: completed]") && lines[0].ends_with("..."), "{}", lines[0]);
    assert!(lines[1].contains("#2") && lines[1].contains("[reject on #1]") && lines[1].contains("reject: no"), "{}", lines[1]);
    assert!(lines[2].contains("[approve on #1, informational]"), "{}", lines[2]);
    assert!(lines[3].contains("[completes #1]") && lines[3].contains("partial"), "{}", lines[3]);
    assert!(lines[4].contains("just a note") && !lines[4].contains('['), "{}", lines[4]);
    assert!(lines[0].chars().filter(|c| *c == '.').count() >= 3);
    // --brief cannot be combined with --json or --tail.
    assert!(!p.run("claude", &["read", "--brief", "--json"], None).status.success());
    assert!(!p.run("claude", &["read", "--brief", "--tail", "5"], None).status.success());
}

#[test]
fn role_prompts_teach_the_loop_without_acknowledgement_posts() {
    let p = Project::new("prompts");
    p.ok("claude", &["new", "r1", "--purpose", "t", "--executor", "claude"]);
    let ex = p.ok("claude", &["prompt", "claude", "--room", "r1"]);
    for phrase in ["--propose", "--complete --re", "never re-request a vote", "A post needs --thoughts unless", "room read --room r1 --brief", "A direct instruction from the user overrides"] {
        assert!(ex.contains(phrase), "executor prompt lacks `{phrase}`:\n{ex}");
    }
    let ad = p.ok("claude", &["prompt", "codex", "--room", "r1"]);
    for phrase in ["--re N --vote", "never post only to confirm", "your counted vote is the acknowledgement", "room feedback bug"] {
        assert!(ad.contains(phrase), "advisor prompt lacks `{phrase}`:\n{ad}");
    }
    assert!(!ex.contains("Only --thoughts is required") && !ad.contains("Only --thoughts is required"));
}
