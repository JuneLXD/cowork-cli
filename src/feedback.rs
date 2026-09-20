//! `cowork feedback`: bug reports and advice for the cowork maintainers.
//!
//! A report is sent only when the user or agent runs this command. The default
//! payload is the report kind, the text, the CLI version, a coarse OS name, and
//! who reported (claude, codex, or human). Project, room, and extra context are
//! opt-in. Reports go to an insert-only table; the key shipped in the binary can
//! write reports but never read them. A report that cannot be sent is queued
//! locally, pinned to the endpoint it was written for, and resent only by an
//! explicit `cowork feedback retry`.

use crate::cli::{FeedbackCmd, ReportArgs};
use crate::{agent, config, paths};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

pub const MAX_TEXT: usize = 8000;
const DEFAULT_TIMEOUT: u64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Report {
    id: String,
    kind: String,
    reporter: String,
    tool: String,
    version: String,
    os: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    room: Option<String>,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
}

/// A queued report remembers where it was meant to go, so a later change of
/// endpoint configuration cannot redirect it.
#[derive(Debug, Serialize, Deserialize)]
struct Queued {
    url: String,
    key: String,
    report: Report,
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub url: String,
    pub key: String,
    pub source: &'static str,
}

pub const KEY_PREFIX: &str = "sb_publishable_";

/// Reports carry free text, so they travel only over TLS. Plain `http://` is
/// allowed solely to a loopback host (tests and local mocks), matched exactly
/// after parsing the URL, not by string prefix.
pub fn check_url(url: &str) -> Result<()> {
    let uri: ureq::http::Uri = url
        .parse()
        .map_err(|e| anyhow!("feedback url `{url}` is not a valid URL: {e}"))?;
    let scheme = uri.scheme_str().unwrap_or("");
    let host = uri.host().unwrap_or("").trim_matches(|c| c == '[' || c == ']');
    if host.is_empty() {
        bail!("feedback url `{url}` has no host");
    }
    match scheme {
        "https" => Ok(()),
        "http" => {
            let loopback = host.eq_ignore_ascii_case("localhost")
                || host.parse::<std::net::Ipv4Addr>().map(|a| a.is_loopback()).unwrap_or(false)
                || host.parse::<std::net::Ipv6Addr>().map(|a| a.is_loopback()).unwrap_or(false);
            if loopback {
                Ok(())
            } else {
                bail!("feedback url `{url}` uses plain http to a non-local host; reports are sent over https only")
            }
        }
        _ => bail!("feedback url `{url}` must use https://"),
    }
}

/// Where reports go: environment, then the project's room.toml, then the values
/// baked in at build time. A source that sets only the url or only the key is an
/// error, never a fallback, so a half-configured override cannot send reports
/// elsewhere. Every key must be a publishable one.
pub fn endpoint() -> Result<Option<Endpoint>> {
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let var = |name: &str| std::env::var(name).ok();
    let env_pair = crate::feedback_env::resolve_pair(
        (var("COWORK_FEEDBACK_URL"), var("COWORK_FEEDBACK_KEY")),
        (var("ROOM_FEEDBACK_URL"), var("ROOM_FEEDBACK_KEY")),
    )
    .map_err(|e| anyhow!("feedback endpoint from the environment is incomplete: {e}"))?;
    let (env_src, env_url, env_key): (&'static str, Option<String>, Option<String>) = match env_pair {
        Some((u, k, src)) => (src, Some(u), Some(k)),
        None => ("environment", None, None),
    };
    let sources: [(&'static str, Option<String>, Option<String>); 3] = [
        (env_src, env_url, env_key),
        {
            let cfg = paths::current_project().ok().map(|p| config::load(&p.root)).unwrap_or_default();
            ("room.toml (feedback_url / feedback_key)", clean(cfg.feedback_url), clean(cfg.feedback_key))
        },
        (
            "built in",
            clean(Some(env!("COWORK_FEEDBACK_URL").to_string())),
            clean(Some(env!("COWORK_FEEDBACK_KEY").to_string())),
        ),
    ];
    for (source, url, key) in sources {
        match (url, key) {
            (None, None) => continue,
            (Some(url), Some(key)) => {
                check_url(&url).map_err(|e| anyhow!("{e} (from {source})"))?;
                if !key.starts_with(KEY_PREFIX) {
                    bail!("feedback key from {source} is not a publishable key (expected a value starting with {KEY_PREFIX}); server-side keys are refused");
                }
                let name: &'static str = if source.starts_with("room.toml") {
                    "room.toml"
                } else if source == "built in" {
                    "built in"
                } else {
                    "environment"
                };
                return Ok(Some(Endpoint { url: url.trim_end_matches('/').to_string(), key, source: name }));
            }
            (u, _) => {
                let missing = if u.is_none() { "url" } else { "key" };
                bail!("feedback endpoint from {source} is incomplete: the {missing} is missing. Set both, or unset both to use the next source.");
            }
        }
    }
    Ok(None)
}

fn queue_dir() -> PathBuf {
    paths::data_dir().join("feedback-queue")
}

fn timeout() -> Duration {
    let secs = std::env::var("COWORK_FEEDBACK_TIMEOUT")
        .ok()
        .or_else(|| std::env::var("ROOM_FEEDBACK_TIMEOUT").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT);
    Duration::from_secs(secs)
}

fn read_text(inline: Option<String>, file: Option<String>, what: &str) -> Result<Option<String>> {
    let text = match (inline, file) {
        (_, Some(f)) if f == "-" => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Some(s)
        }
        (_, Some(f)) => Some(fs::read_to_string(&f).with_context(|| format!("reading {f}"))?),
        (Some(t), None) => Some(t),
        (None, None) => None,
    };
    let text = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if let Some(t) = &text {
        if t.chars().count() > MAX_TEXT {
            bail!("{what} is {} characters; the limit is {MAX_TEXT}. Shorten it or attach the details to a file you can point to.", t.chars().count());
        }
    }
    Ok(text)
}

fn tool_name() -> &'static str {
    if std::env::var_os("CLAUDECODE").is_some() || std::env::var_os("CLAUDE_CODE_ENTRYPOINT").is_some() {
        return "claude-code";
    }
    if std::env::vars_os().any(|(k, _)| {
        let k = k.to_string_lossy();
        k.starts_with("CODEX_") && k != "CODEX_HOME"
    }) {
        return "codex";
    }
    "shell"
}

fn os_name() -> &'static str {
    let v = fs::read_to_string("/proc/version").unwrap_or_default().to_ascii_lowercase();
    if v.contains("microsoft") {
        "wsl"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        std::env::consts::OS
    }
}

/// A random UUID v4 from the kernel, so a retried report has the same primary key.
fn uuid_v4() -> Result<String> {
    let mut b = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}{}{}{}-{}{}-{}{}-{}{}-{}{}{}{}{}{}",
        h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]
    ))
}

enum Outcome {
    Sent,
    Duplicate,
    Failed(String),
}

fn send(url: &str, key: &str, report: &Report) -> Outcome {
    // Redirects are refused: the destination was validated once, and a redirect
    // could send the report somewhere else, possibly over plain http.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(timeout()))
        .build()
        .into();
    let resp = agent
        .post(&format!("{url}/rest/v1/feedback"))
        .header("apikey", key)
        .header("Content-Type", "application/json")
        .header("Prefer", "return=minimal")
        .send_json(report);
    match resp {
        Ok(mut r) => {
            let status = r.status().as_u16();
            let body = r.body_mut().read_to_string().unwrap_or_default();
            match status {
                200..=299 => Outcome::Sent,
                409 if is_duplicate_of_this_report(&body) => Outcome::Duplicate,
                _ => {
                    let snippet: String = body.chars().take(160).collect();
                    Outcome::Failed(format!("HTTP {status} {}", snippet.trim()))
                }
            }
        }
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Only a structured PostgREST error for a unique violation (SQLSTATE 23505) on the
/// table's primary key means this exact report is already stored. Any other 409,
/// or the same code on another constraint, is a failure.
fn is_duplicate_of_this_report(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(body) else { return false };
    let code = v["code"].as_str().unwrap_or("");
    let text = format!("{} {}", v["message"].as_str().unwrap_or(""), v["details"].as_str().unwrap_or(""));
    code == "23505" && text.contains("feedback_pkey")
}

/// The queue holds report text and the endpoint key: owner-only, and an existing
/// directory is tightened as well, not only a freshly created one.
fn private_queue_dir() -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = queue_dir();
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    // Two first-time failures can race here: a concurrent create is fine as long
    // as what exists is a directory. Any other error is reported.
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}
        Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

fn enqueue(url: &str, key: &str, report: &Report) -> Result<PathBuf> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = private_queue_dir()?;
    let p = dir.join(format!("{}.json", report.id));
    let q = Queued { url: url.to_string(), key: key.to_string(), report: report.clone() };
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&p)?;
    f.write_all(serde_json::to_string_pretty(&q)?.as_bytes())?;
    Ok(p)
}

/// Readable queued reports with their paths, plus the paths of files in the queue
/// that could not be parsed (they are reported, never silently ignored or deleted).
type Queue = (Vec<(PathBuf, Queued)>, Vec<PathBuf>);

fn queued() -> Result<Queue> {
    let dir = queue_dir();
    let mut out = Vec::new();
    let mut bad = Vec::new();
    if dir.is_dir() {
        private_queue_dir()?;
    }
    let Ok(rd) = fs::read_dir(&dir) else { return Ok((out, bad)) };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        match fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Queued>(&t).ok()) {
            Some(q) if p.extension().map(|x| x == "json").unwrap_or(false) => out.push((p, q)),
            _ => bad.push(p),
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    bad.sort();
    Ok((out, bad))
}

fn report_bad(bad: &[PathBuf]) {
    for p in bad {
        eprintln!("warning: unreadable file in the feedback queue, left untouched: {}", p.display());
    }
}

fn no_endpoint() -> anyhow::Error {
    anyhow!(
        "no feedback endpoint in this build. Set `cowork config set feedback_url <url>` and `feedback_key <publishable key>`, \
         or export COWORK_FEEDBACK_URL and COWORK_FEEDBACK_KEY, or rebuild with a .env containing project_ID and publishable_key."
    )
}

pub fn run(cmd: FeedbackCmd) -> Result<i32> {
    match cmd {
        FeedbackCmd::Bug(a) => report("bug", a),
        FeedbackCmd::Advice(a) => report("advice", a),
        FeedbackCmd::Retry => retry(),
        FeedbackCmd::List => list(),
        FeedbackCmd::Status => status(),
    }
}

fn report(kind: &str, a: ReportArgs) -> Result<i32> {
    if a.file.as_deref() == Some("-") && a.context_file.as_deref() == Some("-") {
        bail!("stdin (-) can feed either --file or --context-file, not both");
    }
    let message = read_text(a.text, a.file, "the report")?
        .ok_or_else(|| anyhow!("nothing to report: pass the text as an argument, or --file <path>, or --file - and pipe it"))?;
    let context = read_text(a.context, a.context_file, "--context")?;
    let ep = endpoint()?.ok_or_else(no_endpoint)?;

    let (project, room) = if a.project || a.room.is_some() {
        let name = paths::current_project().ok().map(|p| p.name);
        (name, a.room)
    } else {
        (None, None)
    };
    let reporter = agent::detect_env().map(|(n, _)| n).unwrap_or_else(|| "human".to_string());
    let rep = Report {
        id: uuid_v4()?,
        kind: kind.to_string(),
        reporter,
        tool: tool_name().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: os_name().to_string(),
        project,
        room,
        message,
        context,
    };
    if a.dry_run {
        println!("{}", serde_json::to_string_pretty(&rep)?);
        println!("(dry run: nothing sent; endpoint {} from {})", ep.url, ep.source);
        return Ok(0);
    }
    match send(&ep.url, &ep.key, &rep) {
        Outcome::Sent => {
            println!("sent {kind} report {}", rep.id);
            Ok(0)
        }
        Outcome::Duplicate => {
            println!("sent {kind} report {} (it was already stored)", rep.id);
            Ok(0)
        }
        Outcome::Failed(why) => {
            let p = enqueue(&ep.url, &ep.key, &rep)?;
            println!("queued {kind} report {} ({why}); saved at {}. Resend with `cowork feedback retry`.", rep.id, p.display());
            Ok(3)
        }
    }
}

fn retry() -> Result<i32> {
    let (items, bad) = queued()?;
    report_bad(&bad);
    if items.is_empty() {
        println!("nothing queued");
        return Ok(if bad.is_empty() { 0 } else { 3 });
    }
    let mut left = 0;
    for (path, q) in items {
        // The recorded destination is checked again; a report whose url no longer
        // passes is kept untouched and never sent.
        if let Err(e) = check_url(&q.url).and_then(|_| {
            if q.key.starts_with(KEY_PREFIX) { Ok(()) } else { bail!("recorded key is not a publishable key") }
        }) {
            left += 1;
            println!("still queued {} report {} (not sent: {e})", q.report.kind, q.report.id);
            continue;
        }
        match send(&q.url, &q.key, &q.report) {
            Outcome::Sent | Outcome::Duplicate => {
                fs::remove_file(&path)?;
                println!("sent {} report {}", q.report.kind, q.report.id);
            }
            Outcome::Failed(why) => {
                left += 1;
                println!("still queued {} report {} ({why})", q.report.kind, q.report.id);
            }
        }
    }
    if left > 0 || !bad.is_empty() {
        println!("{left} report(s) still queued in {}", queue_dir().display());
        return Ok(3);
    }
    Ok(0)
}

fn list() -> Result<i32> {
    let (items, bad) = queued()?;
    report_bad(&bad);
    if items.is_empty() {
        println!("nothing queued");
        return Ok(if bad.is_empty() { 0 } else { 3 });
    }
    for (_, q) in &items {
        let first = q.report.message.lines().next().unwrap_or("");
        let first: String = first.chars().take(80).collect();
        println!("{}  {:<6}  {}  {}", q.report.id, q.report.kind, q.url, first);
    }
    println!("{} queued in {}; resend with `cowork feedback retry`", items.len(), queue_dir().display());
    Ok(if bad.is_empty() { 0 } else { 3 })
}

fn status() -> Result<i32> {
    match endpoint() {
        Ok(Some(ep)) => println!("endpoint: {} ({})", ep.url, ep.source),
        Ok(None) => println!("endpoint: none ({})", no_endpoint()),
        Err(e) => println!("endpoint: misconfigured ({e})"),
    }
    let (items, bad) = queued()?;
    report_bad(&bad);
    println!("queued: {}", items.len());
    Ok(if bad.is_empty() { 0 } else { 3 })
}

/// For `cowork doctor`.
pub fn describe() -> (bool, String) {
    match endpoint() {
        Ok(Some(ep)) => (true, format!("feedback endpoint configured ({}, from {})", ep.url, ep.source)),
        Ok(None) => (false, "no feedback endpoint in this build; `cowork feedback` will explain how to set one".to_string()),
        Err(e) => (false, format!("feedback endpoint misconfigured: {e}")),
    }
}
