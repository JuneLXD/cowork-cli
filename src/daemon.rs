use crate::{paths, registry};
use anyhow::{anyhow, bail, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const IDLE_EXIT: Duration = Duration::from_secs(3600);

// ---------- client side ----------

pub fn connect() -> Option<UnixStream> {
    UnixStream::connect(paths::socket_path()).ok()
}

fn spawn_detached() -> Result<()> {
    let exe = std::env::current_exe()?;
    fs::create_dir_all(paths::data_dir())?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::log_path())?;
    Command::new(exe)
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()?;
    Ok(())
}

fn last_log_line() -> Option<String> {
    let s = fs::read_to_string(paths::log_path()).ok()?;
    s.lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.to_string())
}

pub fn ensure_running() -> Result<UnixStream> {
    if let Some(s) = connect() {
        return Ok(s);
    }
    spawn_detached()?;
    for _ in 0..60 {
        thread::sleep(Duration::from_millis(50));
        if let Some(s) = connect() {
            return Ok(s);
        }
    }
    let reason = last_log_line()
        .map(|l| format!("; last log line: {l}"))
        .unwrap_or_default();
    Err(anyhow!(
        "daemon did not start (socket {}){reason}. Run `room daemon run` in the foreground to debug; log at {}",
        paths::socket_path().display(),
        paths::log_path().display()
    ))
}

pub fn request(stream: &mut UnixStream, req: Value, timeout: Option<Duration>) -> Result<Value> {
    stream.set_read_timeout(timeout)?;
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(&mut *stream);
    let mut buf = String::new();
    reader.read_line(&mut buf)?;
    if buf.trim().is_empty() {
        bail!("daemon closed the connection");
    }
    Ok(serde_json::from_str(&buf)?)
}

pub fn ping() -> Option<Value> {
    let mut s = connect()?;
    request(&mut s, json!({"cmd": "ping"}), Some(Duration::from_secs(2))).ok()
}

/// Block until `path` changes or `timeout` elapses. Ok(true) on change.
pub fn watch_change(path: &Path, timeout: Duration) -> Result<bool> {
    let mut s = ensure_running()?;
    let r = request(
        &mut s,
        json!({"cmd": "watch", "path": path, "timeout_ms": timeout.as_millis() as u64}),
        Some(timeout + Duration::from_secs(5)),
    )?;
    Ok(r["event"] == "changed")
}

pub fn stop() -> Result<bool> {
    match connect() {
        Some(mut s) => {
            let _ = request(&mut s, json!({"cmd": "shutdown"}), Some(Duration::from_secs(2)));
            Ok(true)
        }
        None => Ok(false),
    }
}

// ---------- server side ----------

struct Notify {
    gens: Mutex<HashMap<PathBuf, u64>>,
    cv: Condvar,
}

struct State {
    n: Arc<Notify>,
    watcher: Mutex<RecommendedWatcher>,
    watched: Mutex<HashSet<PathBuf>>,
    last: Mutex<Instant>,
    active: AtomicUsize,
    started: Instant,
}

fn cleanup() {
    let _ = fs::remove_file(paths::socket_path());
    let _ = fs::remove_file(paths::pid_path());
}

pub fn run_server() -> Result<()> {
    if connect().is_some() {
        return Ok(()); // already running
    }
    let sock = paths::socket_path();
    if let Some(parent) = sock.parent() {
        fs::create_dir_all(parent)?;
    }
    let _ = fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    fs::create_dir_all(paths::data_dir())?;
    fs::write(paths::pid_path(), std::process::id().to_string())?;

    let n = Arc::new(Notify {
        gens: Mutex::new(HashMap::new()),
        cv: Condvar::new(),
    });
    let n2 = n.clone();
    let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(ev) = res {
            let mut g = n2.gens.lock().unwrap();
            for p in ev.paths {
                *g.entry(p).or_insert(0) += 1;
            }
            n2.cv.notify_all();
        }
    })?;
    let state = Arc::new(State {
        n,
        watcher: Mutex::new(watcher),
        watched: Mutex::new(HashSet::new()),
        last: Mutex::new(Instant::now()),
        active: AtomicUsize::new(0),
        started: Instant::now(),
    });

    // Pre-watch every registered project so events are never missed.
    for (_, root) in registry::live_projects() {
        let _ = ensure_watched(&state, &paths::rooms_dir(&root));
    }

    let idle = state.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(30));
        let quiet = idle.active.load(Ordering::SeqCst) == 0
            && idle.last.lock().unwrap().elapsed() > IDLE_EXIT;
        if quiet {
            cleanup();
            std::process::exit(0);
        }
    });

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let st = state.clone();
                thread::spawn(move || handle(&st, stream));
            }
            Err(_) => break,
        }
    }
    cleanup();
    Ok(())
}

fn ensure_watched(state: &State, dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let dir = dir.canonicalize()?;
    let mut w = state.watched.lock().unwrap();
    if w.insert(dir.clone()) {
        state
            .watcher
            .lock()
            .unwrap()
            .watch(&dir, RecursiveMode::NonRecursive)?;
    }
    Ok(())
}

fn handle(state: &Arc<State>, stream: UnixStream) {
    state.active.fetch_add(1, Ordering::SeqCst);
    *state.last.lock().unwrap() = Instant::now();
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_ok() && !line.trim().is_empty() {
        let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let resp = match req["cmd"].as_str() {
            Some("ping") => json!({
                "ok": true,
                "pid": std::process::id(),
                "uptime_s": state.started.elapsed().as_secs(),
                "watched_dirs": state.watched.lock().unwrap().len(),
            }),
            Some("watch") => watch(state, &req),
            Some("register") => {
                let name = req["name"].as_str().unwrap_or("");
                let path = PathBuf::from(req["path"].as_str().unwrap_or(""));
                match registry::register(name, &path) {
                    Ok(()) => {
                        let _ = ensure_watched(state, &paths::rooms_dir(&path));
                        json!({"ok": true})
                    }
                    Err(e) => json!({"error": e.to_string()}),
                }
            }
            Some("projects") => {
                let projects: Vec<Value> = registry::live_projects()
                    .into_iter()
                    .map(|(n, p)| json!({"name": n, "path": p}))
                    .collect();
                json!({"ok": true, "projects": projects})
            }
            Some("shutdown") => {
                let _ = writer.write_all(b"{\"ok\":true}\n");
                cleanup();
                std::process::exit(0);
            }
            _ => json!({"error": "unknown command"}),
        };
        let mut out = resp.to_string();
        out.push('\n');
        let _ = writer.write_all(out.as_bytes());
    }
    state.active.fetch_sub(1, Ordering::SeqCst);
    *state.last.lock().unwrap() = Instant::now();
}

fn watch(state: &State, req: &Value) -> Value {
    let path = PathBuf::from(req["path"].as_str().unwrap_or(""));
    let path = path.canonicalize().unwrap_or(path);
    let timeout = Duration::from_millis(req["timeout_ms"].as_u64().unwrap_or(300_000));
    if let Some(dir) = path.parent() {
        if let Err(e) = ensure_watched(state, dir) {
            return json!({"error": format!("cannot watch {}: {e}", dir.display())});
        }
    }
    let deadline = Instant::now() + timeout;
    let mut g = state.n.gens.lock().unwrap();
    let start = *g.get(&path).unwrap_or(&0);
    loop {
        if *g.get(&path).unwrap_or(&0) != start {
            return json!({"event": "changed"});
        }
        let now = Instant::now();
        if now >= deadline {
            return json!({"event": "timeout"});
        }
        let (g2, _) = state.n.cv.wait_timeout(g, deadline - now).unwrap();
        g = g2;
    }
}
