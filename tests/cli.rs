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
        let p = Self::uninitialized(name);
        let mut args = vec!["init", "--quiet"];
        if let Some(a) = agents {
            args.extend(["--agents", a]);
        }
        let out = p.run("claude", &args, None);
        assert!(out.status.success(), "init failed: {}", String::from_utf8_lossy(&out.stderr));
        p
    }

    fn uninitialized(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("room-cli-test-{}-{}-{}", std::process::id(), n, name));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("xdg-data")).unwrap();
        fs::create_dir_all(root.join("xdg-run")).unwrap();
        Project { root }
    }

    fn cmd(&self, agent: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cowork"));
        c.current_dir(&self.root)
            .env("COWORK_AGENT", agent)
            .env("XDG_DATA_HOME", self.root.join("xdg-data"))
            .env("XDG_RUNTIME_DIR", self.root.join("xdg-run"))
            .env("KIMI_CODE_HOME", self.root.join("kimi-home"))
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .env_remove("ROOM_ID")
            .env_remove("COWORK_ROOM")
            .env_remove("ROOM_AGENT");
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
    c.env("COWORK_FEEDBACK_URL", url)
        .env("COWORK_FEEDBACK_KEY", "sb_publishable_test")
        .env("COWORK_FEEDBACK_TIMEOUT", "1")
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
    let bin = fs::read(env!("CARGO_BIN_EXE_cowork")).unwrap();
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
    let out = p.cmd("codex").env("COWORK_FEEDBACK_URL", &url).env_remove("COWORK_FEEDBACK_KEY").args(["feedback", "bug", "x"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("incomplete"));
    // A half-set primary pair is an error even when a complete legacy pair exists.
    let out = p.cmd("codex").env("COWORK_FEEDBACK_URL", &url).env_remove("COWORK_FEEDBACK_KEY").env("ROOM_FEEDBACK_URL", &url).env("ROOM_FEEDBACK_KEY", "sb_publishable_legacy").args(["feedback", "bug", "x"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("COWORK_FEEDBACK_KEY is missing"));
    // The legacy pair alone still works.
    let out = p.cmd("codex").env_remove("COWORK_FEEDBACK_URL").env_remove("COWORK_FEEDBACK_KEY").env("ROOM_FEEDBACK_URL", &url).env("ROOM_FEEDBACK_KEY", "sb_publishable_legacy").args(["feedback", "bug", "legacy names"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // Only the key set in room.toml.
    p.ok("codex", &["config", "set", "feedback_key", "sb_publishable_abc"]);
    let out = p.cmd("codex").env_remove("COWORK_FEEDBACK_URL").env_remove("COWORK_FEEDBACK_KEY").args(["feedback", "bug", "x"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("incomplete"));
    // A server-side key is refused at runtime as well as at build time.
    p.ok("codex", &["config", "set", "feedback_url", &url]);
    p.ok("codex", &["config", "set", "feedback_key", "sb_secret_nope"]);
    let out = p.cmd("codex").env_remove("COWORK_FEEDBACK_URL").env_remove("COWORK_FEEDBACK_KEY").args(["feedback", "bug", "x"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a publishable key"));
    assert_eq!(seen.lock().unwrap().len(), 1, "only the legacy-pair send went out; every error case sent nothing");
    assert!(queue_files(&p).is_empty(), "nothing was queued either");
    // A complete, valid room.toml pair works.
    let (url2, seen2) = mock_server(201, "", 0, 1);
    p.ok("codex", &["config", "set", "feedback_url", &url2]);
    p.ok("codex", &["config", "set", "feedback_key", "sb_publishable_abc"]);
    let out = p.cmd("codex").env_remove("COWORK_FEEDBACK_URL").env_remove("COWORK_FEEDBACK_KEY").args(["feedback", "bug", "x"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(seen2.lock().unwrap().len(), 1);
    assert_eq!(seen.lock().unwrap().len(), 1, "only the legacy-pair send reached the first server");
    // Source labels: environment for either env pair, room.toml for the config pair.
    let out = p.cmd("codex").env("COWORK_FEEDBACK_URL", &url).env("COWORK_FEEDBACK_KEY", "sb_publishable_x").args(["feedback", "status"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("(environment)"), "{}", String::from_utf8_lossy(&out.stdout));
    let out = p.cmd("codex").env_remove("COWORK_FEEDBACK_URL").env_remove("COWORK_FEEDBACK_KEY").env("ROOM_FEEDBACK_URL", &url).env("ROOM_FEEDBACK_KEY", "sb_publishable_x").args(["feedback", "status"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("(environment)"), "{}", String::from_utf8_lossy(&out.stdout));
    let out = p.cmd("codex").env_remove("COWORK_FEEDBACK_URL").env_remove("COWORK_FEEDBACK_KEY").args(["feedback", "status"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("(room.toml)"), "{}", String::from_utf8_lossy(&out.stdout));
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

fn proposal(s: &serde_json::Value, id: u64) -> &serde_json::Value {
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
    assert!(out.contains("posted #1") && out.contains("(proposal; codex votes with: cowork post --room main --re 1"), "{out}");
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
    assert_eq!(hint, "cowork post --room main --re 1 --vote \"approve: reason\"");
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
    for phrase in ["--propose", "--complete --re", "never re-request a vote", "A post needs --thoughts unless", "cowork read --room r1 --brief", "A direct instruction from the user overrides"] {
        assert!(ex.contains(phrase), "executor prompt lacks `{phrase}`:\n{ex}");
    }
    let ad = p.ok("claude", &["prompt", "codex", "--room", "r1"]);
    for phrase in ["--re N --vote", "never post only to confirm", "your counted vote is the acknowledgement", "cowork feedback bug"] {
        assert!(ad.contains(phrase), "advisor prompt lacks `{phrase}`:\n{ad}");
    }
    assert!(!ex.contains("Only --thoughts is required") && !ad.contains("Only --thoughts is required"));
    // Printing a prompt is read-only for the repository: the saved copy is untouched.
    let saved = p.root.join(".ai-common/prompts/r1.md");
    fs::write(&saved, "kept as is").unwrap();
    p.ok("claude", &["prompt", "claude", "--room", "r1"]);
    assert_eq!(fs::read_to_string(&saved).unwrap(), "kept as is");
    assert!(!p.run("claude", &["prompt", "nobody", "--room", "r1"], None).status.success());
    assert_eq!(fs::read_to_string(&saved).unwrap(), "kept as is", "an invalid agent changes nothing either");
}

// ---------- rename: upgrade paths ----------

#[test]
fn legacy_room_agent_and_room_id_still_identify_the_caller_and_room() {
    let p = Project::new("legacy-env");
    let out = p.cmd("ignored").env_remove("COWORK_AGENT").env("ROOM_AGENT", "codex").args(["post", "--thoughts", "via legacy name"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("as codex"));
    p.ok("claude", &["new", "side", "--purpose", "t"]);
    let out = p.cmd("claude").env("ROOM_ID", "side").args(["post", "--thoughts", "into side via ROOM_ID"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("/side as claude"), "{}", String::from_utf8_lossy(&out.stdout));
    let out = p.cmd("claude").env("COWORK_ROOM", "side").env("ROOM_ID", "main").args(["post", "--thoughts", "COWORK_ROOM wins"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("/side as claude"));
}

#[test]
fn old_hook_entries_are_replaced_not_duplicated() {
    let p = Project::new("legacy-hooks");
    let f = p.root.join(".claude/settings.json");
    let old = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"room hook stop --agent claude","timeout":150}]}],"UserPromptSubmit":[{"hooks":[{"type":"command","command":"room hook prompt --agent claude","timeout":20}]}],"SessionStart":[{"hooks":[{"type":"command","command":"room hook session --agent claude","timeout":20}]}]},"other":true}"#;
    fs::write(&f, old).unwrap();
    assert!(p.ok("claude", &["hook", "status"]).contains("claude  installed"), "old entries count as installed");
    p.ok("claude", &["hook", "install", "--tool", "claude"]);
    let text = fs::read_to_string(&f).unwrap();
    assert_eq!(text.matches("hook stop").count(), 1, "no duplicate Stop entry:\n{text}");
    assert!(text.contains("cowork hook stop --agent claude") && !text.contains("room hook"), "{text}");
    assert!(text.contains("\"other\": true"), "unrelated settings kept");
    p.ok("claude", &["hook", "remove", "--tool", "claude"]);
    let text = fs::read_to_string(&f).unwrap();
    assert!(!text.contains("hook") && text.contains("other"), "{text}");
}

#[test]
fn old_rules_block_is_replaced_by_one_cowork_block() {
    let p = Project::new("legacy-rules");
    let f = p.root.join("CLAUDE.md");
    fs::write(&f, "# Mine\n\nkeep this\n\n<!-- room-cli:start -->\n## room-cli: coordinating with codex\nold text\n<!-- room-cli:end -->\n\nand this\n").unwrap();
    p.ok("claude", &["init", "--quiet"]);
    let text = fs::read_to_string(&f).unwrap();
    assert_eq!(text.matches("<!-- cowork:start -->").count(), 1, "{text}");
    assert!(!text.contains("room-cli:start") && !text.contains("old text"), "{text}");
    assert!(text.contains("keep this") && text.contains("and this"), "{text}");
    assert!(text.contains("## cowork: coordinating with codex"), "{text}");
    // Re-running init on the new block keeps a single block too.
    p.ok("claude", &["init", "--quiet"]);
    assert_eq!(fs::read_to_string(&f).unwrap().matches("<!-- cowork:start -->").count(), 1);
}

#[test]
fn room_alias_binary_is_the_same_program() {
    let p = Project::new("alias");
    let alias = std::path::Path::new(env!("CARGO_BIN_EXE_cowork")).with_file_name("room");
    assert!(alias.exists(), "alias binary built next to cowork");
    let out = Command::new(&alias)
        .current_dir(&p.root)
        .env("COWORK_AGENT", "codex")
        .env("XDG_DATA_HOME", p.root.join("xdg-data"))
        .env("XDG_RUNTIME_DIR", p.root.join("xdg-run"))
        .args(["post", "--thoughts", "from the alias"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(p.ok("claude", &["read"]).contains("from the alias"));
    let v = Command::new(&alias).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&v.stdout).starts_with("cowork "));
}

#[test]
fn generated_prompts_and_protocol_use_the_new_name() {
    let p = Project::new("new-name");
    let proto = fs::read_to_string(p.root.join(".ai-common/PROTOCOL.md")).unwrap();
    assert!(proto.contains("`cowork`") && !proto.contains("`room post") && !proto.contains("room-cli"), "{proto}");
    let k = p.ok("claude", &["prompt", "claude"]);
    assert!(k.contains("`cowork` CLI") && k.contains("COWORK_AGENT=claude") && !k.contains("ROOM_AGENT"), "{k}");
    let rules = fs::read_to_string(p.root.join("AGENTS.md")).unwrap();
    assert!(rules.contains("## cowork: coordinating with claude") && rules.contains("cowork feedback bug"), "{rules}");
}

#[test]
fn init_keeps_a_customized_protocol_file() {
    let p = Project::new("custom-protocol");
    let f = p.root.join(".ai-common/PROTOCOL.md");
    let mut text = fs::read_to_string(&f).unwrap();
    text.push_str("\n## House rule\n\nAlways run the linter first.\n");
    fs::write(&f, &text).unwrap();
    p.ok("claude", &["init", "--quiet"]);
    assert_eq!(fs::read_to_string(&f).unwrap(), text, "init is create-only for PROTOCOL.md");
}

#[test]
fn init_can_skip_agent_files_and_use_the_shared_protocol() {
    let p = Project::uninitialized("skip-agent-files");
    let out = p.ok("claude", &["init", "--no-agent-files"]);
    assert!(out.contains("agent files: skipped"), "{out}");
    assert!(!out.contains("rules already in"), "{out}");
    for file in ["AGENTS.md", "CLAUDE.md"] {
        assert!(!p.root.join(file).exists(), "{file} must not be created");
    }
    for file in [".ai-common/rooms/main.md", ".ai-common/PROTOCOL.md", ".ai-common/ONBOARDING.md", ".claude/settings.json", ".codex/hooks.json"] {
        assert!(p.root.join(file).is_file(), "{file} must still be created");
    }
    let onboarding = fs::read_to_string(p.root.join(".ai-common/ONBOARDING.md")).unwrap();
    assert!(onboarding.contains("Follow the cowork rules in `.ai-common/PROTOCOL.md`"), "{onboarding}");
    for agent in ["claude", "codex"] {
        let prompt = p.ok(agent, &["prompt", agent]);
        assert!(prompt.contains("Follow the cowork rules in `.ai-common/PROTOCOL.md`"), "{prompt}");
        let hook = p.ok(agent, &["hook", "session", "--agent", agent]);
        assert!(hook.contains("Rules: `.ai-common/PROTOCOL.md`"), "{hook}");
    }
    let doctor = p.ok("claude", &["doctor"]);
    assert!(doctor.contains("[info] optional rules block"), "{doctor}");
    assert!(!doctor.lines().any(|l| l.contains("[warn]") && l.contains("rules block")), "{doctor}");
}

#[test]
fn skipping_agent_files_preserves_custom_text_and_existing_blocks() {
    let p = Project::uninitialized("keep-agent-files");
    let custom = "# My project instructions\nKeep this exactly.\n";
    for file in ["AGENTS.md", "CLAUDE.md"] {
        fs::write(p.root.join(file), custom).unwrap();
    }
    p.ok("claude", &["init", "--no-agent-files", "--quiet"]);
    p.ok("claude", &["new", "custom", "--no-agent-files"]);
    for file in ["AGENTS.md", "CLAUDE.md"] {
        assert_eq!(fs::read_to_string(p.root.join(file)).unwrap(), custom);
    }
    assert!(p.ok("claude", &["prompt", "claude"]).contains("`.ai-common/PROTOCOL.md`"));
    p.ok("claude", &["init", "--agent-files", "--quiet"]);
    let before: Vec<_> = ["AGENTS.md", "CLAUDE.md"].iter().map(|f| fs::read(p.root.join(f)).unwrap()).collect();
    p.ok("claude", &["init", "--no-agent-files", "--quiet"]);
    p.ok("claude", &["new", "existing", "--no-agent-files"]);
    for (file, content) in ["AGENTS.md", "CLAUDE.md"].iter().zip(before) {
        assert_eq!(fs::read(p.root.join(file)).unwrap(), content);
    }
}

#[test]
fn new_room_can_create_agent_files_after_skipping_setup() {
    let p = Project::uninitialized("room-agent-files");
    p.ok("claude", &["init", "--no-agent-files", "--quiet"]);
    for args in [vec!["new", "default"], vec!["new", "skipped", "--no-agent-files"]] {
        let out = p.ok("claude", &args);
        assert!(out.contains("Project-wide cowork rules are in `.ai-common/PROTOCOL.md`"), "{out}");
        assert!(!p.root.join("AGENTS.md").exists());
        assert!(!p.root.join("CLAUDE.md").exists());
    }
    let custom = "# Keep my own rules\n";
    fs::write(p.root.join("AGENTS.md"), custom).unwrap();
    p.ok("claude", &["new", "with-files", "--agent-files", "--executor", "codex"]);
    p.ok("claude", &["new", "refresh-files", "--agent-files"]);
    for file in ["AGENTS.md", "CLAUDE.md"] {
        let text = fs::read_to_string(p.root.join(file)).unwrap();
        assert_eq!(text.matches("<!-- cowork:start -->").count(), 1, "{text}");
        assert_eq!(text.matches("<!-- cowork:end -->").count(), 1, "{text}");
    }
    assert!(fs::read_to_string(p.root.join("AGENTS.md")).unwrap().starts_with(custom));
    let saved = fs::read_to_string(p.root.join(".ai-common/prompts/with-files.md")).unwrap();
    assert!(saved.contains("Project-wide cowork rules are in `AGENTS.md`"), "{saved}");
    assert!(saved.contains("Project-wide cowork rules are in `CLAUDE.md`"), "{saved}");
    assert!(saved.contains("the EXECUTOR in room"), "{saved}");
}

#[test]
fn agent_file_flags_conflict_before_any_files_are_written() {
    let p = Project::uninitialized("agent-files-conflict");
    let out = p.run("claude", &["init", "--agent-files", "--no-agent-files"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(!p.root.join(".ai-common").exists());
    p.ok("claude", &["init", "--no-agent-files", "--quiet"]);
    let out = p.run("claude", &["new", "conflict", "--agent-files", "--no-agent-files"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(!p.root.join(".ai-common/rooms/conflict.md").exists());
    assert!(!p.root.join("AGENTS.md").exists());
    assert!(!p.root.join("CLAUDE.md").exists());
}

#[test]
fn agent_file_generation_respects_custom_participants() {
    let p = Project::uninitialized("custom-agent-files");
    p.ok("claude", &["init", "--agents", "codex,reviewer", "--no-agent-files", "--quiet"]);
    p.ok("codex", &["new", "custom", "--agent-files"]);
    assert!(!p.root.join("CLAUDE.md").exists());
    let rules = fs::read_to_string(p.root.join("AGENTS.md")).unwrap();
    assert!(rules.contains("`codex` or `reviewer`"), "{rules}");
    assert_eq!(rules.matches("<!-- cowork:start -->").count(), 1);
}

#[test]
fn hook_upgrade_keeps_unrelated_entries_in_a_mixed_group() {
    let p = Project::new("mixed-hooks");
    let f = p.root.join(".claude/settings.json");
    let mixed = r#"{"hooks":{"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"room hook stop --agent claude","timeout":150},{"type":"command","command":"say done","timeout":5}]}],"PreToolUse":[{"hooks":[{"type":"command","command":"lint"}]}]}}"#;
    fs::write(&f, mixed).unwrap();
    p.ok("claude", &["hook", "install", "--tool", "claude"]);
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&f).unwrap()).unwrap();
    let stop = v["hooks"]["Stop"].as_array().unwrap();
    let text = v.to_string();
    assert!(!text.contains("room hook"), "legacy entry replaced: {text}");
    assert_eq!(text.matches("cowork hook stop").count(), 1, "{text}");
    assert!(text.contains("say done") && text.contains("\"matcher\":\"*\""), "unrelated entry and group metadata kept: {text}");
    assert!(text.contains("lint"), "other events untouched");
    assert_eq!(stop.len(), 2, "the mixed group survives beside our own group: {text}");
    p.ok("claude", &["hook", "remove", "--tool", "claude"]);
    let text = fs::read_to_string(&f).unwrap();
    assert!(!text.contains("hook stop") && text.contains("say done") && text.contains("lint"), "{text}");
}

// ---------- feedback hardening ----------

#[test]
fn queued_reports_are_owner_only_even_in_an_existing_open_directory() {
    use std::os::unix::fs::PermissionsExt;
    let p = Project::new("fb-perms");
    let dir = p.root.join("xdg-data/room/feedback-queue");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let (down, _) = mock_server(500, "", 0, 1);
    assert_eq!(feedback(&p, &down, &["bug", "kept private"], None).status.code(), Some(3));
    assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "existing directory tightened");
    for f in queue_files(&p) {
        assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600, "{}", f.display());
    }
}

#[test]
fn plaintext_http_is_refused_except_to_loopback() {
    let p = Project::new("fb-http");
    for bad in ["http://example.com/x", "http://127.0.0.1.evil.example/x", "http://localhost.example/x", "ftp://127.0.0.1/x", "https://"] {
        let out = feedback(&p, bad, &["bug", "x"], None);
        assert!(!out.status.success(), "{bad} must be refused");
        assert!(queue_files(&p).is_empty(), "{bad}: nothing queued");
    }
    // Loopback over plain http (the mock servers) still works, as the other tests show.
    let (url, seen) = mock_server(201, "", 0, 1);
    assert!(feedback(&p, &url, &["bug", "local"], None).status.success());
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn retry_refuses_a_queued_report_whose_recorded_url_is_unsafe() {
    let p = Project::new("fb-retry-url");
    let (down, _) = mock_server(500, "", 0, 1);
    assert_eq!(feedback(&p, &down, &["bug", "later"], None).status.code(), Some(3));
    let f = &queue_files(&p)[0];
    let mut q: serde_json::Value = serde_json::from_str(&fs::read_to_string(f).unwrap()).unwrap();
    q["url"] = serde_json::Value::String("http://example.com".into());
    fs::write(f, q.to_string()).unwrap();
    let (good, seen) = mock_server(201, "", 0, 1);
    let out = feedback(&p, &good, &["retry"], None);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stdout).contains("not sent"), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(queue_files(&p).len(), 1, "kept untouched");
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn redirects_are_not_followed() {
    let p = Project::new("fb-redirect");
    let (target, target_seen) = mock_server(201, "", 0, 1);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let redirect_url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let resp = format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {target}/rest/v1/feedback\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    let out = feedback(&p, &redirect_url, &["bug", "do not follow"], None);
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stdout).contains("HTTP 307"));
    assert!(target_seen.lock().unwrap().is_empty(), "redirect target never contacted");
    assert_eq!(queue_files(&p).len(), 1);
}

#[test]
fn concurrent_first_failures_all_enqueue() {
    let p = Project::new("fb-race");
    let (down, _) = mock_server(500, "", 0, 8);
    let mut children = Vec::new();
    for i in 0..8 {
        let mut c = p.cmd("codex");
        c.env("COWORK_FEEDBACK_URL", &down).env("COWORK_FEEDBACK_KEY", "sb_publishable_test").env("COWORK_FEEDBACK_TIMEOUT", "2");
        c.args(["feedback", "bug", &format!("race {i}")]).stdout(Stdio::piped()).stderr(Stdio::piped());
        children.push(c.spawn().unwrap());
    }
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    }
    assert_eq!(queue_files(&p).len(), 8, "every failed report was queued");
}

#[test]
fn bare_cowork_starts_with_the_intro_and_intro_replays_it() {
    let p = Project::new("intro");
    let intro = p.ok("claude", &["intro"]);
    assert!(intro.lines().count() >= 10 && intro.lines().all(|l| l.chars().count() <= 76), "{intro}");
    assert!(intro.contains("cowork init") && intro.contains("cowork status"), "{intro}");
    assert!(intro.lines().next().unwrap().starts_with(&format!("cowork {} - ", env!("CARGO_PKG_VERSION"))), "version on the first line: {intro}");
    // Piped bare `cowork` in an initialised project: intro first, then the status block.
    let bare = String::from_utf8_lossy(&p.run("claude", &[], None).stdout).to_string();
    assert!(bare.starts_with(intro.trim_end()), "intro comes first:\n{bare}");
    assert!(bare.contains("last post by") || bare.contains("no posts yet") || bare.contains("/main"), "status follows:\n{bare}");
    // Before init, the intro still comes first, then the setup hint.
    let fresh = std::env::temp_dir().join(format!("cowork-intro-fresh-{}", std::process::id()));
    let _ = fs::remove_dir_all(&fresh);
    fs::create_dir_all(fresh.join(".git")).unwrap();
    // An empty .ai-common pins project discovery here, so an initialised ancestor cannot capture it.
    fs::create_dir_all(fresh.join(".ai-common")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cowork")).current_dir(&fresh).env("XDG_DATA_HOME", &fresh).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with(intro.trim_end()), "{text}");
    assert!(text.contains("cowork is not set up here"), "the setup hint follows the banner:\n{text}");
    let _ = fs::remove_dir_all(&fresh);
    // Explicit subcommands and JSON stay free of the banner.
    let status = p.ok("claude", &["status", "--json"]);
    assert!(status.trim_start().starts_with('['), "{status}");
    assert!(!p.ok("claude", &["read", "--last", "1"]).contains("helps coding agents"));
}

// ---------- kimi and room rosters ----------

fn room_status_json(p: &Project, room: &str) -> serde_json::Value {
    let text = p.ok("claude", &["status", "--room", room, "--json"]);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v[0].clone()
}

#[test]
fn three_agent_role_prompts_name_one_executor_and_the_fellow_advisor() {
    let p = Project::with_agents("trio-prompts", Some("claude,codex,kimi"));
    p.ok("claude", &["new", "trio", "--executor", "claude"]);
    let ex = p.ok("claude", &["prompt", "claude", "--room", "trio"]);
    for phrase in [
        "working with `codex` and `kimi` (the advisors)",
        "approved by one advisor's approve and blocked by any advisor's reject",
        "for `codex` or `kimi`'s vote",
        "When `codex` or `kimi` posts a proposal",
    ] {
        assert!(ex.contains(phrase), "executor prompt lacks `{phrase}`:\n{ex}");
    }
    for (me, peer) in [("codex", "kimi"), ("kimi", "codex")] {
        let ad = p.ok("claude", &["prompt", me, "--room", "trio"]);
        assert!(ad.contains("working with `claude` (the executor)") && ad.contains("`claude` makes every change"), "{me}:\n{ad}");
        assert!(ad.contains(&format!("`{peer}` is a fellow advisor")), "{me} names {peer}:\n{ad}");
        assert!(!ad.contains("`codex, kimi`") && !ad.contains("`claude, "), "{me} sees one executor:\n{ad}");
    }
    let kimi = p.ok("claude", &["prompt", "kimi", "--room", "trio"]);
    assert!(kimi.contains("Kimi Code specifics") && kimi.contains("COWORK_AGENT=kimi cowork wait --room trio"), "{kimi}");
    let kickoff = p.ok("claude", &["prompt", "kimi"]);
    assert!(kickoff.contains("Kimi Code specifics") && kickoff.contains("COWORK_AGENT=kimi cowork read --room main"), "{kickoff}");
    // Two-agent rooms read as before.
    p.ok("claude", &["new", "duo", "--agents", "claude,codex"]);
    let ex2 = p.ok("claude", &["prompt", "claude", "--room", "duo"]);
    assert!(ex2.contains("working with `codex` (the advisor). ") && ex2.contains("for `codex`'s vote"), "{ex2}");
    assert!(!ex2.contains("advisors") && !ex2.contains("fellow advisor"), "{ex2}");
    let ad2 = p.ok("claude", &["prompt", "codex", "--room", "duo"]);
    assert!(!ad2.contains("fellow advisor"), "{ad2}");
}

#[test]
fn new_agents_sets_this_rooms_roster_only() {
    let p = Project::new("roster-new");
    p.ok("claude", &["new", "pair", "--agents", "claude, kimi", "--executor", "kimi"]);
    let text = fs::read_to_string(p.root.join(".ai-common/rooms/pair.md")).unwrap();
    assert!(text.contains("participants: claude, kimi\n") && text.contains("executor: kimi\n"), "{text}");
    assert!(p.ok("claude", &["prompt", "claude", "--room", "pair"]).contains("working with `kimi` (the executor)"));
    assert!(!p.run("claude", &["prompt", "codex", "--room", "pair"], None).status.success(), "codex is not in this room");
    // The project's own list and main are untouched.
    assert!(!p.ok("claude", &["config", "get", "agents"]).contains("kimi"));
    assert!(fs::read_to_string(p.room_file()).unwrap().contains("participants: claude, codex\n"));
}

#[test]
fn bad_rosters_are_rejected_before_anything_is_written() {
    let p = Project::new("roster-bad");
    let cases: &[(&[&str], &str)] = &[
        (&["--agents", "claude"], "2 to 3 agents"),
        (&["--agents", "claude,codex,kimi,gemini"], "2 to 3 agents"),
        (&["--agents", "claude,Claude"], "listed twice"),
        (&["--agents", "claude,kimi", "--executor", "codex"], "not one of this room's agents"),
        (&["--executor", "kimi"], "not one of this room's agents"),
    ];
    for (i, (extra, why)) in cases.iter().enumerate() {
        let name = format!("bad{i}");
        let mut args = vec!["new", name.as_str()];
        args.extend_from_slice(extra);
        let out = p.run("claude", &args, None);
        assert!(!out.status.success(), "{args:?} should fail");
        assert!(String::from_utf8_lossy(&out.stderr).contains(why), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(!p.root.join(format!(".ai-common/rooms/{name}.md")).exists(), "{args:?} left a room");
        assert!(!p.root.join(format!(".ai-common/prompts/{name}.md")).exists(), "{args:?} left prompts");
    }
    // An inherited project list is checked the same way.
    p.ok("claude", &["config", "set", "agents", "claude,codex,kimi,gemini"]);
    let out = p.run("claude", &["new", "inherited"], None);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("2 to 3 agents"));
    assert!(!p.root.join(".ai-common/rooms/inherited.md").exists());
}

#[test]
fn hooks_deliver_only_rooms_the_agent_belongs_to() {
    let p = Project::new("roster-hooks");
    p.ok("claude", &["new", "pair", "--agents", "claude,kimi"]);
    p.ok("kimi", &["post", "--room", "pair", "--thoughts", "kimi only talks to claude here"]);
    let cwd = format!("{{\"cwd\": {:?}}}", p.root.to_string_lossy());
    let codex = p.run("codex", &["hook", "prompt", "--agent", "codex"], Some(&cwd));
    assert!(!String::from_utf8_lossy(&codex.stdout).contains("kimi only talks"), "codex is not in pair");
    let stop = p.run("codex", &["hook", "stop", "--agent", "codex", "--timeout", "1"], Some(&cwd));
    assert!(stop.stdout.is_empty(), "no stop block for a non-member: {}", String::from_utf8_lossy(&stop.stdout));
    let session = String::from_utf8_lossy(&p.run("codex", &["hook", "session", "--agent", "codex"], Some(&cwd)).stdout).to_string();
    assert!(session.contains("room `main`") && !session.contains("room `pair`"), "{session}");
    let claude = p.run("claude", &["hook", "prompt", "--agent", "claude"], Some(&cwd));
    assert!(String::from_utf8_lossy(&claude.stdout).contains("kimi only talks"), "claude is in pair");
    let session = String::from_utf8_lossy(&p.run("kimi", &["hook", "session", "--agent", "kimi"], Some(&cwd)).stdout).to_string();
    assert!(session.contains("room `pair`") && !session.contains("room `main`"), "kimi is in pair, not main: {session}");
    // Manual reads are unchanged: anyone can still look.
    assert!(p.ok("codex", &["read", "--room", "pair", "--last", "1"]).contains("kimi only talks"));
}

#[test]
fn three_agent_votes_one_approve_approves_and_any_reject_blocks() {
    let p = Project::with_agents("trio-votes", Some("claude,codex,kimi"));
    p.ok("claude", &["new", "trio", "--executor", "claude"]);
    p.ok("claude", &["post", "--room", "trio", "--propose", "--thoughts", "t", "--action", "x"]);
    assert_eq!(proposal(&room_status_json(&p, "trio"), 1)["waiting_for"], serde_json::json!(["codex", "kimi"]));
    p.ok("kimi", &["post", "--room", "trio", "--re", "1", "--vote", "approve: fine"]);
    let s = room_status_json(&p, "trio");
    assert_eq!(proposal(&s, 1)["state"], "approved");
    assert_eq!(proposal(&s, 1)["waiting_for"], serde_json::json!(["codex"]));
    p.ok("codex", &["post", "--room", "trio", "--re", "1", "--vote", "reject: missing tests"]);
    assert_eq!(proposal(&room_status_json(&p, "trio"), 1)["state"], "rejected");
}

#[test]
fn kimi_wait_sees_later_posts_skips_its_own_and_keeps_its_own_cursor() {
    let p = Project::with_agents("trio-wait", Some("claude,codex,kimi"));
    p.ok("claude", &["new", "trio"]);
    p.ok("kimi", &["post", "--room", "trio", "--thoughts", "hello from kimi"]);
    let out = p.run("kimi", &["wait", "--room", "trio", "--timeout", "1"], None);
    assert_eq!(out.status.code(), Some(2), "own post does not wake kimi");
    // A post that lands while kimi is already waiting.
    let child = p
        .cmd("kimi")
        .args(["wait", "--room", "trio", "--timeout", "10"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    p.ok("codex", &["post", "--room", "trio", "--thoughts", "later from codex"]);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("later from codex") && !text.contains("hello from kimi"), "{text}");
    assert_eq!(p.run("kimi", &["wait", "--room", "trio", "--timeout", "1"], None).status.code(), Some(2), "cursor advanced");
    // codex's cursor is its own: it still has kimi's post to read.
    let codex = p.ok("codex", &["read", "--room", "trio"]);
    assert!(codex.contains("hello from kimi") && !codex.contains("later from codex"), "{codex}");
    let cursors = p.root.join(".ai-common/.cursors");
    assert!(cursors.join("kimi/trio").exists() && cursors.join("codex/trio").exists());
}

#[test]
fn init_checks_the_roster_before_writing_anything() {
    for list in ["claude", "claude,codex,kimi,gemini", "claude,Claude"] {
        let p = Project::uninitialized("init-roster");
        let out = p.run("claude", &["init", "--quiet", "--agents", list], None);
        assert!(!out.status.success(), "{list} should fail");
        assert!(!p.root.join(".ai-common").exists(), "{list} wrote files");
        assert!(!p.root.join("AGENTS.md").exists() && !p.root.join(".claude").exists(), "{list} wrote files");
    }
    // An inherited list is checked when it is about to become main's roster.
    let p = Project::uninitialized("init-inherited");
    fs::create_dir_all(p.root.join(".ai-common")).unwrap();
    fs::write(p.root.join(".ai-common/room.toml"), "agents = [\"claude\", \"codex\", \"kimi\", \"gemini\"]\n").unwrap();
    let out = p.run("claude", &["init", "--quiet"], None);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("2 to 3 agents"));
    assert!(!p.root.join(".ai-common/rooms/main.md").exists());
    // Three is fine.
    let p = Project::with_agents("init-trio", Some("claude,codex,kimi"));
    assert!(fs::read_to_string(p.room_file()).unwrap().contains("participants: claude, codex, kimi\n"));
}

#[test]
fn kickoff_prompts_are_only_for_mains_members() {
    let p = Project::new("kickoff-members");
    p.ok("claude", &["config", "set", "agents", "claude,codex,kimi"]);
    let out = p.run("claude", &["prompt", "kimi"], None);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("not a member of room main") && err.contains("cowork new"), "{err}");
    p.ok("claude", &["new", "pair", "--agents", "claude,kimi"]);
    let err = String::from_utf8_lossy(&p.run("claude", &["prompt", "kimi"], None).stderr).to_string();
    assert!(err.contains("`cowork prompt kimi --room pair`"), "{err}");
    assert!(p.ok("claude", &["prompt", "codex"]).contains("You are `codex` in room"));
    // Re-running init leaves main's roster alone and does not hand kimi a main prompt.
    p.ok("claude", &["init", "--quiet", "--no-agent-files"]);
    assert!(fs::read_to_string(p.room_file()).unwrap().contains("participants: claude, codex\n"));
    let onboarding = fs::read_to_string(p.root.join(".ai-common/ONBOARDING.md")).unwrap();
    assert!(onboarding.contains("## codex") && onboarding.contains("## kimi"), "{onboarding}");
    assert!(!onboarding.contains("You are `kimi` in room") && onboarding.contains("cowork prompt kimi --room pair"), "{onboarding}");
}

#[test]
fn agent_files_keep_speaking_for_agents_still_in_rooms() {
    let p = Project::new("files-union");
    p.ok("claude", &["new", "side", "--agents", "codex,kimi", "--no-agent-files"]);
    p.ok("claude", &["init", "--quiet", "--agents", "claude,kimi", "--agent-files"]);
    let agents_md = fs::read_to_string(p.root.join("AGENTS.md")).unwrap();
    assert!(agents_md.contains("`codex`") && agents_md.contains("`kimi`"), "codex is still in main and side:\n{agents_md}");
    assert!(fs::read_to_string(p.room_file()).unwrap().contains("participants: claude, codex\n"));
    let side = fs::read_to_string(p.root.join(".ai-common/rooms/side.md")).unwrap();
    assert!(side.contains("participants: codex, kimi\n"), "{side}");
}

// ---------- kimi code hooks ----------

fn kimi_config(p: &Project) -> PathBuf {
    p.root.join("kimi-home/config.toml")
}

const USER_KIMI_CONFIG: &str = "# my settings\ndefault_model = \"k2\"\n\n[[hooks]]\nevent = \"PreToolUse\"\nmatcher = \"Bash\"\ncommand = \"node check.mjs\"\ntimeout = 5\n";

#[test]
fn kimi_hooks_are_installed_only_when_named_and_keep_the_users_config() {
    let p = Project::with_agents("kimi-install", Some("claude,codex,kimi"));
    assert!(!p.root.join("kimi-home").exists(), "init never writes the global kimi config");
    let out = p.ok("claude", &["hook", "install"]);
    assert!(!p.root.join("kimi-home").exists(), "a default install leaves it alone too");
    assert!(out.contains("cowork hook install --tool kimi"), "{out}");
    p.ok("claude", &["hook", "install", "--tool", "all"]);
    assert!(!p.root.join("kimi-home").exists(), "`all` means the project-level tools");

    fs::create_dir_all(p.root.join("kimi-home")).unwrap();
    fs::write(kimi_config(&p), USER_KIMI_CONFIG).unwrap();
    p.ok("claude", &["hook", "install", "--tool", "kimi"]);
    let text = fs::read_to_string(kimi_config(&p)).unwrap();
    assert!(text.starts_with(USER_KIMI_CONFIG), "user text is kept verbatim:\n{text}");
    let cfg: toml::Table = text.parse().unwrap();
    let hooks = cfg["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 3);
    let ours: Vec<&toml::Table> = hooks.iter().map(|h| h.as_table().unwrap()).filter(|h| h["command"].as_str().unwrap().starts_with("cowork hook ")).collect();
    let events: Vec<&str> = ours.iter().map(|h| h["event"].as_str().unwrap()).collect();
    assert_eq!(events, ["Stop", "UserPromptSubmit"]);
    for h in &ours {
        assert!(h.keys().all(|k| ["event", "matcher", "command", "timeout"].contains(&k.as_str())), "{h:?}");
        assert!(h["command"].as_str().unwrap().contains("--agent kimi --format kimi"), "{h:?}");
        assert!(h["timeout"].as_integer().unwrap() <= 600, "{h:?}");
    }
    assert_eq!(ours[0]["command"].as_str().unwrap(), "cowork hook stop --agent kimi --format kimi --timeout 120");
    assert_eq!(ours[0]["timeout"].as_integer(), Some(150));

    p.ok("claude", &["hook", "install", "--tool", "kimi"]);
    assert_eq!(fs::read_to_string(kimi_config(&p)).unwrap(), text, "reinstall is byte-identical");
    let status = p.ok("claude", &["hook", "status"]);
    assert!(status.lines().any(|l| l.starts_with("kimi") && l.contains("installed") && l.contains("(global)")), "{status}");

    p.ok("claude", &["hook", "remove"]);
    assert_eq!(fs::read_to_string(kimi_config(&p)).unwrap(), text, "a default remove leaves kimi alone");
    p.ok("claude", &["hook", "remove", "--tool", "kimi"]);
    assert_eq!(fs::read_to_string(kimi_config(&p)).unwrap(), USER_KIMI_CONFIG, "remove restores the user's file exactly");

    // A long hook_wait is capped under Kimi Code's 600-second hook limit.
    p.ok("claude", &["config", "set", "hook_wait", "900"]);
    p.ok("claude", &["hook", "install", "--tool", "kimi"]);
    let cfg: toml::Table = fs::read_to_string(kimi_config(&p)).unwrap().parse().unwrap();
    let stop = cfg["hooks"].as_array().unwrap().iter().map(|h| h.as_table().unwrap()).find(|h| h["event"].as_str() == Some("Stop") && h["command"].as_str().unwrap().starts_with("cowork")).unwrap().clone();
    assert!(stop["command"].as_str().unwrap().ends_with("--timeout 570") && stop["timeout"].as_integer() == Some(600), "{stop:?}");
}

#[test]
fn kimi_hook_install_refuses_configs_it_cannot_edit_safely() {
    let p = Project::with_agents("kimi-refuse", Some("claude,codex,kimi"));
    fs::create_dir_all(p.root.join("kimi-home")).unwrap();
    let block = "# cowork:start (managed by cowork)\n[[hooks]]\nevent = \"Stop\"\ncommand = \"cowork hook stop --agent kimi --format kimi\"\n# cowork:end\n";
    let cases = [
        ("invalid TOML", "model = = \"k2\"\n".to_string()),
        ("inline hooks", "hooks = []\n".to_string()),
        ("hooks not tables", "hooks = [1, 2]\n".to_string()),
        ("start without end", "# cowork:start\n[[hooks]]\nevent = \"Stop\"\ncommand = \"x\"\n".to_string()),
        ("two blocks", format!("{block}\n{block}")),
        ("stray cowork entry", "[[hooks]]\nevent = \"Stop\"\ncommand = \"cowork hook stop --agent kimi\"\n".to_string()),
    ];
    for (what, text) in &cases {
        fs::write(kimi_config(&p), text).unwrap();
        for action in ["install", "remove"] {
            let out = p.run("claude", &["hook", action, "--tool", "kimi"], None);
            // Remove has nothing to do in a valid file without a cowork block.
            if action == "install" || *what != "inline hooks" {
                assert!(!out.status.success(), "{what}: {action} should fail");
                assert!(String::from_utf8_lossy(&out.stderr).contains("left unchanged"), "{what}: {}", String::from_utf8_lossy(&out.stderr));
            }
            assert_eq!(&fs::read_to_string(kimi_config(&p)).unwrap(), text, "{what}: {action} changed the file");
        }
    }
    // A config that cannot be read is an error, never an empty file to overwrite.
    fs::remove_file(kimi_config(&p)).unwrap();
    fs::create_dir(kimi_config(&p)).unwrap();
    let out = p.run("claude", &["hook", "install", "--tool", "kimi"], None);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("cannot read"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(kimi_config(&p).is_dir());
}

#[test]
fn kimi_hooks_speak_exit_code_two_and_plain_text() {
    let p = Project::with_agents("kimi-protocol", Some("claude,codex,kimi"));
    let payload = format!("{{\"hook_event_name\": \"Stop\", \"session_id\": \"s1\", \"cwd\": {:?}, \"stop_hook_active\": false}}", p.root.to_string_lossy());
    // Nothing unread: allow the stop, silently.
    p.ok("kimi", &["read"]);
    let out = p.run("kimi", &["hook", "stop", "--agent", "kimi", "--format", "kimi", "--timeout", "1"], Some(&payload));
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "silent timeout");
    // A message that arrives while the hook waits: exit 2, reason on stderr, nothing on stdout.
    let mut child = p
        .cmd("kimi")
        .args(["hook", "stop", "--agent", "kimi", "--format", "kimi", "--timeout", "10"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    p.post("codex", "kimi, please review a.rs");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stderr).contains("kimi, please review a.rs"));
    // Delivered once: the next stop is quiet again.
    let out = p.run("kimi", &["hook", "stop", "--agent", "kimi", "--format", "kimi", "--timeout", "1"], Some(&payload));
    assert_eq!(out.status.code(), Some(0));
    // UserPromptSubmit: plain text on stdout, not JSON.
    p.post("claude", "kimi, one more thing");
    let out = p.run("kimi", &["hook", "prompt", "--agent", "kimi", "--format", "kimi"], Some(&payload));
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("kimi, one more thing") && !text.trim_start().starts_with('{'), "{text}");
    // Claude and Codex keep their JSON.
    p.post("kimi", "for claude");
    let out = p.run("claude", &["hook", "prompt", "--agent", "claude"], Some(&payload));
    assert!(String::from_utf8_lossy(&out.stdout).trim_start().starts_with('{'));
}

#[test]
fn a_global_hook_returns_at_once_where_its_agent_is_in_no_room() {
    let p = Project::new("kimi-outsider");
    let payload = format!("{{\"cwd\": {:?}}}", p.root.to_string_lossy());
    p.post("codex", "not for kimi");
    let start = std::time::Instant::now();
    let out = p.run("kimi", &["hook", "stop", "--agent", "kimi", "--format", "kimi"], Some(&payload));
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty() && out.stdout.is_empty());
    assert!(start.elapsed() < std::time::Duration::from_secs(3), "took {:?} with the default 120 s wait", start.elapsed());
    let out = p.run("kimi", &["hook", "prompt", "--agent", "kimi", "--format", "kimi"], Some(&payload));
    assert!(out.stdout.is_empty(), "kimi is in no room here");
}

#[test]
fn rooms_point_at_the_hooks_their_agents_still_need() {
    let p = Project::with_agents("room-hints", Some("claude,kimi"));
    assert!(!p.root.join(".codex/hooks.json").exists(), "codex is not on the project list");
    const CODEX_HINT: &str = "room hooks for codex are not installed; run `cowork hook install --tool codex`";
    const KIMI_HINT: &str = "kimi: Kimi Code reads hooks only from its global config";
    let out = p.ok("claude", &["new", "side", "--agents", "codex,kimi", "--no-agent-files"]);
    assert!(out.contains(CODEX_HINT), "room-only codex needs its hooks:\n{out}");
    assert!(out.contains(KIMI_HINT), "{out}");
    // The default install covers agents that are only in rooms.
    p.ok("claude", &["hook", "install"]);
    assert!(p.root.join(".codex/hooks.json").exists());
    let out = p.ok("claude", &["new", "side2", "--agents", "codex,claude", "--no-agent-files"]);
    assert!(!out.contains("are not installed; run"), "{out}");
    p.ok("claude", &["hook", "install", "--tool", "kimi"]);
    let out = p.ok("claude", &["new", "side3", "--agents", "claude,kimi", "--no-agent-files"]);
    assert!(!out.contains(KIMI_HINT), "{out}");
    let doctor = p.ok("claude", &["doctor"]);
    assert!(doctor.contains("room hooks for kimi installed") && doctor.contains("room hooks for codex installed"), "{doctor}");
}

#[test]
fn kimi_hook_install_and_remove_restore_any_file_ending_byte_for_byte() {
    let p = Project::with_agents("kimi-bytes", Some("claude,codex,kimi"));
    fs::create_dir_all(p.root.join("kimi-home")).unwrap();
    let body = "# mine\nmodel = \"k2\"";
    let originals = [
        String::new(),
        body.to_string(),
        format!("{body}\n"),
        format!("{body}\n\n"),
        format!("{body}\n\n\n"),
    ];
    for original in &originals {
        fs::write(kimi_config(&p), original).unwrap();
        p.ok("claude", &["hook", "install", "--tool", "kimi"]);
        let once = fs::read_to_string(kimi_config(&p)).unwrap();
        assert!(once.starts_with(original.as_str()), "install only appends: {original:?}\n{once}");
        p.ok("claude", &["hook", "install", "--tool", "kimi"]);
        assert_eq!(fs::read_to_string(kimi_config(&p)).unwrap(), once, "reinstall is byte-identical for {original:?}");
        p.ok("claude", &["hook", "remove", "--tool", "kimi"]);
        assert_eq!(&fs::read_to_string(kimi_config(&p)).unwrap(), original, "remove restores {original:?}");
    }
}

#[test]
fn kimi_hook_ownership_is_proven_not_guessed() {
    let p = Project::with_agents("kimi-owner", Some("claude,codex,kimi"));
    fs::create_dir_all(p.root.join("kimi-home")).unwrap();
    p.ok("claude", &["hook", "install", "--tool", "kimi"]);
    let good = fs::read_to_string(kimi_config(&p)).unwrap();
    let (block_start, _) = good.split_once("[[hooks]]").unwrap();
    let cases = [
        // Marker lines inside a multi-line string are text, not a block.
        ("markers in a string", format!("notes = \"\"\"\n{good}\"\"\"\n")),
        ("markers around a user note in a string", "notes = \"\"\"\n# cowork:start pad=0\nremember the release notes\n# cowork:end\n\"\"\"\n".to_string()),
        // A block whose entries were deleted by hand.
        ("emptied block", format!("{block_start}# cowork:end\n")),
        // Top-level keys after the block fold into its last table.
        ("keys leaking into the block", format!("{good}extra = 1\n")),
    ];
    for (what, text) in &cases {
        fs::write(kimi_config(&p), text).unwrap();
        for action in ["install", "remove"] {
            let out = p.run("claude", &["hook", action, "--tool", "kimi"], None);
            assert!(!out.status.success(), "{what}: {action} should refuse");
            assert_eq!(&fs::read_to_string(kimi_config(&p)).unwrap(), text, "{what}: {action} changed the file");
        }
        let status = p.ok("claude", &["hook", "status"]);
        let kimi_line = status.lines().find(|l| l.starts_with("kimi")).unwrap();
        assert!(!kimi_line.contains(" installed "), "{what}: reported installed: {kimi_line}");
    }
}

#[test]
fn stop_hook_names_each_room_separately_and_tells_kimi_to_keep_waiting() {
    let p = Project::with_agents("two-rooms", Some("claude,codex,kimi"));
    p.ok("claude", &["new", "pair", "--no-agent-files"]);
    let payload = format!("{{\"cwd\": {:?}}}", p.root.to_string_lossy());
    for me in ["claude", "kimi"] {
        p.ok(me, &["read"]);
        p.ok(me, &["read", "--room", "pair"]);
    }
    p.post("codex", "in main");
    p.ok("codex", &["post", "--room", "pair", "--thoughts", "in pair"]);

    let out = p.run("kimi", &["hook", "stop", "--agent", "kimi", "--format", "kimi", "--timeout", "1"], Some(&payload));
    assert_eq!(out.status.code(), Some(2));
    let reason = String::from_utf8_lossy(&out.stderr);
    assert!(reason.contains("in main") && reason.contains("in pair"), "{reason}");
    assert!(reason.contains("COWORK_AGENT=kimi cowork post --room main ") && reason.contains("COWORK_AGENT=kimi cowork post --room pair "), "{reason}");
    assert!(!reason.contains("main|pair") && !reason.contains("pair|main"), "{reason}");
    assert!(reason.contains("once per turn") && reason.contains("cowork wait --all-rooms --timeout 600") && reason.contains("WaitFor"), "{reason}");
    assert!(!reason.contains("you may stop"), "{reason}");

    let out = p.run("claude", &["hook", "stop", "--agent", "claude", "--timeout", "1"], Some(&payload));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("`cowork post --room main ") && reason.contains("`cowork post --room pair "), "{reason}");
    assert!(!reason.contains("main|pair") && reason.contains("you may stop"), "{reason}");
}

#[test]
fn wait_all_rooms_covers_only_the_callers_rooms() {
    let p = Project::new("wait-members");
    p.ok("claude", &["new", "a", "--agents", "claude,kimi", "--no-agent-files"]);
    p.ok("claude", &["new", "b", "--agents", "codex,kimi", "--no-agent-files"]);
    let wait = |timeout: &str| p.run("kimi", &["wait", "--all-rooms", "--timeout", timeout], None);
    assert_eq!(wait("1").status.code(), Some(2));
    let cursors = p.root.join(".ai-common/.cursors/kimi");
    assert!(cursors.join("a").exists() && cursors.join("b").exists() && !cursors.join("main").exists());
    // A post in main, which kimi is not in, does not wake it.
    p.post("codex", "main only");
    assert_eq!(wait("1").status.code(), Some(2));
    assert!(!cursors.join("main").exists());
    // Posts in its rooms do, including one that lands mid-wait after a main post.
    p.ok("codex", &["post", "--room", "b", "--thoughts", "in b"]);
    let out = wait("1");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("in b"));
    let child = p.cmd("kimi").args(["wait", "--all-rooms", "--timeout", "10"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    p.post("codex", "main again");
    std::thread::sleep(std::time::Duration::from_millis(300));
    p.ok("claude", &["post", "--room", "a", "--thoughts", "in a"]);
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0));
    assert!(text.contains("in a") && !text.contains("main again"), "{text}");
    // An agent in no room gets an error, not an endless wait.
    let out = p.run("gemini", &["wait", "--all-rooms", "--timeout", "1"], None);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("not a member of any room"));
    // Explicit --room is still open for manual inspection: it listens from now, like any first wait.
    assert_eq!(p.run("kimi", &["wait", "--room", "main", "--timeout", "1"], None).status.code(), Some(2));
    assert!(cursors.join("main").exists());
}
