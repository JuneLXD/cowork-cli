use anyhow::{bail, Context, Result};
use chrono::Utc;
use fs2::FileExt;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const F_THOUGHTS: &str = "Thoughts & Insight";
pub const F_ACTION: &str = "Proposed Action";
pub const F_TAKEN: &str = "Action Taken / Code Changes";
pub const F_HANDOFF: &str = "Handoff / Questions for Counterpart";
pub const F_VOTE: &str = "Vote";
pub const F_ID: &str = "Id";
pub const F_RE: &str = "Re";
pub const F_PROPOSAL: &str = "Proposal";
pub const F_COMPLETES: &str = "Completes";
pub const NONE: &str = "None";
pub const HEADER_PREFIX: &str = "### [";

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    /// Per-room sequence number, allocated under the append lock. Messages
    /// written before ids existed have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// The id of the message this one answers (a vote on a proposal, a reply).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub re: Option<u64>,
    /// Posted with --propose: a plan that needs a vote before it is acted on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub proposal: bool,
    /// Posted with --complete --re P: explicitly closes proposal P.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completes: Option<u64>,
    pub agent: String,
    pub timestamp: String,
    pub thoughts: String,
    pub action: String,
    pub taken: String,
    pub handoff: String,
    /// Optional: `approve`, `reject`, or `abstain`, with a reason. Empty when absent.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub vote: String,
}

impl Message {
    pub fn header(&self) -> String {
        header_line(&self.agent, &self.timestamp)
    }

    /// What read cursors store: the id when there is one, else the header line.
    /// Ids are unique; headers collide when one agent posts twice in a second.
    pub fn cursor_token(&self) -> String {
        match self.id {
            Some(id) => format!("id:{id}"),
            None => self.header(),
        }
    }

    pub fn render(&self) -> String {
        self.render_with(true)
    }

    /// The message for a reader: every marker, but only fields with content.
    /// The file always keeps every field (see `render`), so nothing is lost.
    pub fn render_display(&self) -> String {
        self.render_with(false)
    }

    /// Field-aware rendering. A field is omitted (when `all` is false) only if
    /// its complete trimmed value is empty or `None`; multi-line values whose
    /// first line happens to be "None" are kept whole.
    fn render_with(&self, all: bool) -> String {
        let mut s = String::new();
        s.push_str(&self.header());
        s.push_str("\n\n");
        if let Some(id) = self.id {
            s.push_str(&format!("- **{F_ID}:** {id}\n"));
        }
        if let Some(re) = self.re {
            s.push_str(&format!("- **{F_RE}:** #{re}\n"));
        }
        if self.proposal {
            s.push_str(&format!("- **{F_PROPOSAL}:** yes\n"));
        }
        if let Some(c) = self.completes {
            s.push_str(&format!("- **{F_COMPLETES}:** #{c}\n"));
        }
        for (k, v) in [
            (F_THOUGHTS, &self.thoughts),
            (F_ACTION, &self.action),
            (F_TAKEN, &self.taken),
            (F_HANDOFF, &self.handoff),
        ] {
            if all || !is_none(v) {
                s.push_str(&format!("- **{}:** {}\n", k, indent(v)));
            }
        }
        if !self.vote.trim().is_empty() {
            s.push_str(&format!("- **{}:** {}\n", F_VOTE, indent(&self.vote)));
        }
        s.push('\n');
        s
    }

    /// `approve`, `reject`, or `abstain` when the vote has a recognised prefix.
    pub fn decision(&self) -> Option<&'static str> {
        let w = self.vote.trim().split(':').next()?.trim().to_ascii_lowercase();
        match w.as_str() {
            "approve" => Some("approve"),
            "reject" => Some("reject"),
            "abstain" => Some("abstain"),
            _ => None,
        }
    }

    pub fn has_handoff(&self) -> bool {
        let h = self.handoff.trim();
        !h.is_empty() && !h.eq_ignore_ascii_case(NONE)
    }

    fn field_mut(&mut self, key: &str) -> Option<&mut String> {
        match key {
            F_THOUGHTS => Some(&mut self.thoughts),
            F_ACTION => Some(&mut self.action),
            F_TAKEN => Some(&mut self.taken),
            F_HANDOFF => Some(&mut self.handoff),
            F_VOTE => Some(&mut self.vote),
            _ => None,
        }
    }

    fn trim_fields(&mut self) {
        for f in [
            &mut self.thoughts,
            &mut self.action,
            &mut self.taken,
            &mut self.handoff,
            &mut self.vote,
        ] {
            let t = f.trim_end().to_string();
            *f = t;
        }
    }
}

/// True when a field carries nothing: empty, or exactly `None` (any case).
pub fn is_none(v: &str) -> bool {
    let t = v.trim();
    t.is_empty() || t.eq_ignore_ascii_case(NONE)
}

fn indent(v: &str) -> String {
    let v = v.trim();
    if v.is_empty() {
        NONE.to_string()
    } else {
        v.replace('\n', "\n  ")
    }
}

pub fn header_line(agent: &str, ts: &str) -> String {
    format!("{HEADER_PREFIX}{agent}] - {ts}")
}

pub fn now_ts() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

pub fn validate_agent(name: &str) -> Result<()> {
    let n = name.trim();
    if n.is_empty() || n.contains(']') || n.contains('\n') || n.contains('/') {
        bail!("invalid agent name `{name}`: must be non-empty and contain no `]`, `/` or newline");
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct FrontMatter {
    pub room: String,
    pub project: String,
    pub created: String,
    pub purpose: String,
    pub participants: Vec<String>,
    /// The one agent allowed to change files in this room. Empty means unassigned.
    pub executor: String,
    /// High-water mark of allocated message ids, kept here so `archive` cannot
    /// cause ids to be reused. Ids are contiguous from 1: every id in 1..=last_id
    /// existed at some point, even if it has since been archived.
    pub last_id: u64,
}

impl FrontMatter {
    pub fn render(&self) -> String {
        let mut s = format!(
            "---\nroom: {}\nproject: {}\ncreated: {}\npurpose: {}\nparticipants: {}\nexecutor: {}\n",
            self.room,
            self.project,
            self.created,
            self.purpose,
            self.participants.join(", "),
            self.executor
        );
        if self.last_id > 0 {
            s.push_str(&format!("last_id: {}\n", self.last_id));
        }
        s.push_str("---\n\n");
        s
    }
}

#[derive(Debug, Default)]
pub struct RoomFile {
    pub front: Option<FrontMatter>,
    /// Text between the front matter and the first message (archive stubs), newline-terminated.
    pub head: String,
    pub messages: Vec<Message>,
}

impl RoomFile {
    /// The room's participants: its front matter when present, else the
    /// project-wide list. Every command that judges membership uses this, so
    /// post, status, and archive can never disagree.
    pub fn participants(&self, fallback: &[String]) -> Vec<String> {
        self.front
            .as_ref()
            .map(|f| f.participants.clone())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| fallback.to_vec())
    }

    /// Highest id ever allocated in this room: the front-matter mark or the
    /// highest id still in the file, whichever is greater.
    pub fn last_id(&self) -> u64 {
        let in_file = self.messages.iter().filter_map(|m| m.id).max().unwrap_or(0);
        self.front.as_ref().map(|f| f.last_id).unwrap_or(0).max(in_file)
    }
}

fn parse_front(lines: &[&str]) -> FrontMatter {
    let mut fm = FrontMatter::default();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let v = v.trim().to_string();
            match k.trim() {
                "room" => fm.room = v,
                "project" => fm.project = v,
                "created" => fm.created = v,
                "purpose" => fm.purpose = v,
                "executor" => fm.executor = v,
                "last_id" => fm.last_id = v.parse().unwrap_or(0),
                "participants" => {
                    fm.participants = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                _ => {}
            }
        }
    }
    fm
}

fn parse_header(line: &str) -> Message {
    let rest = &line[HEADER_PREFIX.len()..];
    let (agent, ts) = match rest.find("] - ") {
        Some(p) => (rest[..p].to_string(), rest[p + 4..].trim().to_string()),
        None => (rest.trim_end_matches(']').trim().to_string(), String::new()),
    };
    Message {
        id: None,
        re: None,
        proposal: false,
        completes: None,
        agent,
        timestamp: ts,
        thoughts: String::new(),
        action: String::new(),
        taken: String::new(),
        handoff: String::new(),
        vote: String::new(),
    }
}

pub fn parse(content: &str) -> RoomFile {
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    let mut front = None;
    if lines.first().map(|l| l.trim() == "---").unwrap_or(false) {
        if let Some(end) = lines.iter().skip(1).position(|l| l.trim() == "---") {
            front = Some(parse_front(&lines[1..1 + end]));
            i = end + 2;
        }
    }
    let first_msg = lines
        .iter()
        .enumerate()
        .skip(i)
        .find(|(_, l)| l.starts_with(HEADER_PREFIX))
        .map(|(n, _)| n)
        .unwrap_or(lines.len());
    let head: String = lines[i..first_msg].iter().map(|l| format!("{l}\n")).collect();

    let mut messages: Vec<Message> = Vec::new();
    let mut cur: Option<Message> = None;
    let mut field: Option<String> = None;
    for line in &lines[first_msg..] {
        if line.starts_with(HEADER_PREFIX) {
            if let Some(mut m) = cur.take() {
                m.trim_fields();
                messages.push(m);
            }
            cur = Some(parse_header(line));
            field = None;
            continue;
        }
        let Some(m) = cur.as_mut() else { continue };
        if let Some(rest) = line.strip_prefix("- **") {
            if let Some(p) = rest.find(":**") {
                let key = rest[..p].to_string();
                let value = rest[p + 3..].trim_start().to_string();
                if key == F_ID {
                    m.id = value.trim().parse().ok();
                    field = None;
                    continue;
                }
                if key == F_RE {
                    m.re = value.trim().trim_start_matches('#').parse().ok();
                    field = None;
                    continue;
                }
                if key == F_PROPOSAL {
                    m.proposal = value.trim().eq_ignore_ascii_case("yes");
                    field = None;
                    continue;
                }
                if key == F_COMPLETES {
                    m.completes = value.trim().trim_start_matches('#').parse().ok();
                    field = None;
                    continue;
                }
                if let Some(f) = m.field_mut(&key) {
                    *f = value;
                    field = Some(key);
                } else {
                    field = None;
                }
                continue;
            }
        }
        if let Some(k) = &field {
            let cont = line.strip_prefix("  ").unwrap_or(line);
            if let Some(f) = m.field_mut(k) {
                f.push('\n');
                f.push_str(cont);
            }
        }
    }
    if let Some(mut m) = cur.take() {
        m.trim_fields();
        messages.push(m);
    }
    RoomFile {
        front,
        head,
        messages,
    }
}

/// Read the whole file under a shared lock.
pub fn read_locked(path: &Path) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    f.lock_shared()?;
    let mut s = String::new();
    let r = f.read_to_string(&mut s);
    let _ = f.unlock();
    r?;
    Ok(s)
}

/// Append under an exclusive lock. The closure sees the file as it is while the
/// lock is held and returns the text to append, so anything derived from the
/// current content (the next message id, a reply target check) is atomic with
/// the write. An error from the closure appends nothing.
pub fn append_locked<F>(path: &Path, make: F) -> Result<()>
where
    F: FnOnce(&str) -> Result<String>,
{
    let mut f = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.lock_exclusive()?;
    let res = (|| -> Result<()> {
        let mut content = String::new();
        f.seek(SeekFrom::Start(0))?;
        f.read_to_string(&mut content)?;
        let text = make(&content)?;
        let prefix = if !content.is_empty() && !content.ends_with('\n') { "\n" } else { "" };
        f.write_all(prefix.as_bytes())?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        Ok(())
    })();
    let _ = f.unlock();
    res
}

/// Rewrite a file under an exclusive lock. The closure returns `None` to leave it untouched.
pub fn rewrite_locked<F>(path: &Path, f: F) -> Result<bool>
where
    F: FnOnce(&str) -> Result<Option<String>>,
{
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.lock_exclusive()?;
    let res = (|| -> Result<bool> {
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        match f(&content)? {
            None => Ok(false),
            Some(new) => {
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                file.write_all(new.as_bytes())?;
                file.sync_all()?;
                Ok(true)
            }
        }
    })();
    let _ = file.unlock();
    res
}

pub fn last_lines(content: &str, n: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(n);
    let mut s = lines[start..].join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}
