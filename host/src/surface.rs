//! The `pi` surface — the core half. Owns everything the guest must not:
//! the clock, the timer wheel, HTTP streaming threads, the sandboxed file
//! root, the env allowlist, and the RNG. Ops are synchronous (Law 2:
//! intent crosses as ops); every fact flows back as a per-tick event batch
//! built by the pump in main.rs (facts cross as events).

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rquickjs::Function;
use serde_json::{json, Value};

use crate::guest::Guest;

/// Facts produced by core threads, drained by the pump into event batches.
pub enum FetchEvent {
    Status { id: u32, status: u16, headers: Vec<(String, String)> },
    Chunk { id: u32, text: String },
    Done { id: u32 },
    Error { id: u32, message: String, aborted: bool },
}

pub struct HostState {
    started: Instant,
    /// (fires_at_monotonic_ms, id) — a small wheel; agent runs have few timers.
    pub timers: Vec<(u64, u32)>,
    pub inflight: HashMap<u32, Arc<AtomicBool>>,
    pub fetch_rx: Receiver<FetchEvent>,
    fetch_tx: Sender<FetchEvent>,
    pub exit_code: Option<i32>,
    root: PathBuf,
    env: HashMap<String, String>,
    args: Vec<String>,
    rng: u64,
}

impl HostState {
    pub fn new(root: PathBuf, env: HashMap<String, String>, args: Vec<String>, seed: u64) -> Rc<RefCell<HostState>> {
        let (fetch_tx, fetch_rx) = channel();
        Rc::new(RefCell::new(HostState {
            started: Instant::now(),
            timers: Vec::new(),
            inflight: HashMap::new(),
            fetch_rx,
            fetch_tx,
            exit_code: None,
            root,
            env,
            args,
            rng: seed | 1,
        }))
    }

    pub fn monotonic_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn next_random(&mut self) -> f64 {
        // xorshift64* — deterministic under --seed, good enough for jitter.
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        ((x.wrapping_mul(0x2545F4914F6CDD1D)) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Resolve a guest path inside the sandbox root; rejects escapes.
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let rel = Path::new(path);
        let mut out = self.root.clone();
        for component in rel.components() {
            match component {
                Component::Normal(part) => out.push(part),
                Component::CurDir => {}
                Component::RootDir | Component::Prefix(_) => {
                    return Err(format!("pi.fs: absolute paths are outside the sandbox ({path})"))
                }
                Component::ParentDir => {
                    if !out.pop() || !out.starts_with(&self.root) {
                        return Err(format!("pi.fs: path escapes the sandbox root ({path})"));
                    }
                }
            }
        }
        if !out.starts_with(&self.root) {
            return Err(format!("pi.fs: path escapes the sandbox root ({path})"));
        }
        Ok(out)
    }
}

/// True when nothing can ever wake the guest again (pump exit condition).
pub fn quiescent(state: &HostState) -> bool {
    state.timers.is_empty() && state.inflight.is_empty()
}

fn start_fetch(state: &mut HostState, id: u32, req_json: String) {
    let abort = Arc::new(AtomicBool::new(false));
    state.inflight.insert(id, abort.clone());
    let tx = state.fetch_tx.clone();
    std::thread::spawn(move || run_fetch(id, req_json, tx, abort));
}

fn run_fetch(id: u32, req_json: String, tx: Sender<FetchEvent>, abort: Arc<AtomicBool>) {
    let fail = |tx: &Sender<FetchEvent>, message: String| {
        let _ = tx.send(FetchEvent::Error { id, message, aborted: false });
    };
    let req: Value = match serde_json::from_str(&req_json) {
        Ok(v) => v,
        Err(e) => return fail(&tx, format!("bad request json: {e}")),
    };
    let url = req["url"].as_str().unwrap_or_default().to_string();
    let method = req["method"].as_str().unwrap_or("GET").to_uppercase();
    let mut request = ureq::agent().request(&method, &url);
    if let Some(headers) = req["headers"].as_object() {
        for (k, v) in headers {
            if let Some(v) = v.as_str() {
                request = request.set(k, v);
            }
        }
    }
    let sent = match req["body"].as_str() {
        Some(body) => request.send_string(body),
        None => request.call(),
    };
    let response = match sent {
        Ok(r) => r,
        // 4xx/5xx still carry a response body worth streaming to the guest.
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return fail(&tx, e.to_string()),
    };
    let status = response.status();
    let headers: Vec<(String, String)> = response
        .headers_names()
        .into_iter()
        .filter_map(|name| response.header(&name).map(|v| (name.to_lowercase(), v.to_string())))
        .collect();
    if tx.send(FetchEvent::Status { id, status, headers }).is_err() {
        return;
    }
    let mut reader = response.into_reader();
    let mut buf = [0u8; 8192];
    loop {
        if abort.load(Ordering::Relaxed) {
            let _ = tx.send(FetchEvent::Error { id, message: "aborted".into(), aborted: true });
            return;
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                if tx.send(FetchEvent::Chunk { id, text }).is_err() {
                    return;
                }
            }
            Err(e) => return fail(&tx, e.to_string()),
        }
    }
    let _ = tx.send(FetchEvent::Done { id });
}

pub fn fetch_event_json(event: &FetchEvent) -> Value {
    match event {
        FetchEvent::Status { id, status, headers } => {
            let map: serde_json::Map<String, Value> = headers
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            json!({"kind": "fetchStatus", "id": id, "status": status, "headers": map})
        }
        FetchEvent::Chunk { id, text } => json!({"kind": "fetchChunk", "id": id, "text": text}),
        FetchEvent::Done { id } => json!({"kind": "fetchDone", "id": id}),
        FetchEvent::Error { id, message, aborted } => {
            json!({"kind": "fetchError", "id": id, "message": message, "aborted": aborted})
        }
    }
}

/// Mount the `pi` surface. Every op is a plain function on `globalThis.pi`.
pub fn mount(guest: &Guest, state: Rc<RefCell<HostState>>) -> Result<()> {
    guest.mount(crate::spec_generated::SURFACE, |ctx, ns| {
        macro_rules! op {
            ($name:literal, $f:expr) => {
                ns.set($name, Function::new(ctx.clone(), $f)?)?;
            };
        }

        {
            op!("now", || -> f64 {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as f64)
                    .unwrap_or(0.0)
            });
        }
        {
            let st = state.clone();
            op!("monotonic", move || -> f64 { st.borrow().monotonic_ms() as f64 });
        }
        {
            let st = state.clone();
            op!("random", move || -> f64 { st.borrow_mut().next_random() });
        }
        {
            let st = state.clone();
            op!("timerStart", move |id: u32, ms: f64| {
                let mut s = st.borrow_mut();
                let at = s.monotonic_ms().saturating_add(ms.max(0.0) as u64);
                s.timers.push((at, id));
            });
        }
        {
            let st = state.clone();
            op!("timerClear", move |id: u32| {
                st.borrow_mut().timers.retain(|(_, t)| *t != id);
            });
        }
        {
            let st = state.clone();
            op!("fetchStart", move |id: u32, req: String| {
                start_fetch(&mut st.borrow_mut(), id, req);
            });
        }
        {
            let st = state.clone();
            op!("fetchAbort", move |id: u32| {
                if let Some(flag) = st.borrow().inflight.get(&id) {
                    flag.store(true, Ordering::Relaxed);
                }
            });
        }
        {
            let st = state.clone();
            op!("fsRead", move |path: String| -> rquickjs::Result<Option<String>> {
                let s = st.borrow();
                match s.resolve(&path) {
                    Ok(p) => Ok(std::fs::read_to_string(p).ok()),
                    Err(_) => Ok(None),
                }
            });
        }
        {
            let st = state.clone();
            op!("fsWrite", move |path: String, text: String| -> rquickjs::Result<bool> {
                let s = st.borrow();
                let Ok(p) = s.resolve(&path) else { return Ok(false) };
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                Ok(std::fs::write(p, text).is_ok())
            });
        }
        {
            let st = state.clone();
            op!("fsAppend", move |path: String, text: String| -> rquickjs::Result<bool> {
                let s = st.borrow();
                let Ok(p) = s.resolve(&path) else { return Ok(false) };
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                use std::io::Write;
                let file = std::fs::OpenOptions::new().create(true).append(true).open(p);
                Ok(match file {
                    Ok(mut f) => f.write_all(text.as_bytes()).is_ok(),
                    Err(_) => false,
                })
            });
        }
        {
            let st = state.clone();
            op!("fsExists", move |path: String| -> bool {
                let s = st.borrow();
                s.resolve(&path).map(|p| p.exists()).unwrap_or(false)
            });
        }
        {
            let st = state.clone();
            op!("fsRemove", move |path: String| {
                let s = st.borrow();
                if let Ok(p) = s.resolve(&path) {
                    let _ = std::fs::remove_file(p);
                }
            });
        }
        {
            let st = state.clone();
            op!("env", move |name: String| -> Option<String> {
                st.borrow().env.get(&name).cloned()
            });
        }
        {
            let st = state.clone();
            op!("args", move || -> String {
                serde_json::to_string(&st.borrow().args).unwrap_or_else(|_| "[]".into())
            });
        }
        {
            let st = state.clone();
            op!("exit", move |code: i32| {
                st.borrow_mut().exit_code = Some(code);
            });
        }
        Ok(())
    })
}
