//! pocket-pi — run a bundled agent program on the `pi` surface.
//!
//!   pocket-pi run <bundle.js> [--root DIR] [--allow-env NAME]...
//!                 [--env K=V]... [--seed N] [-- guest args...]
//!
//! The pump (Law 3, agent-domain form): evaluate the bundle (the boot
//! turn), then loop — collect facts (due timers, fetch progress) into ONE
//! batch, deliver it as ONE guest turn via __pi_dispatch, drain jobs —
//! until the world is quiescent: no due or pending timers, no inflight
//! requests, no exit requested. Quiescence-driven exit is what makes
//! headless agent runs (and CI goldens) deterministic.

mod guest;
mod spec_generated;
mod surface;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

use guest::Guest;
use surface::{fetch_event_json, quiescent, FetchEvent, HostState};

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pocket-pi: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let mut argv = std::env::args().skip(1);
    match argv.next().as_deref() {
        Some("run") => {}
        Some("spec") => {
            println!("surface {} v{}", spec_generated::SURFACE, spec_generated::VERSION);
            println!("ops:    {}", spec_generated::OP_NAMES.join(", "));
            println!("events: {}", spec_generated::EVENT_NAMES.join(", "));
            return Ok(0);
        }
        _ => return Err(anyhow!("usage: pocket-pi run <bundle.js> [--root DIR] [--allow-env NAME] [--env K=V] [--seed N] [-- args]")),
    }

    let rest: Vec<String> = argv.collect();
    let mut bundle_path: Option<String> = None;
    let mut root = std::env::current_dir()?;
    let mut env: HashMap<String, String> = HashMap::new();
    let mut seed: u64 = 0x9E3779B97F4A7C15;
    let mut guest_args: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--root" => {
                i += 1;
                root = std::path::PathBuf::from(rest.get(i).ok_or_else(|| anyhow!("--root needs a value"))?);
            }
            "--allow-env" => {
                i += 1;
                let name = rest.get(i).ok_or_else(|| anyhow!("--allow-env needs a name"))?;
                if let Ok(value) = std::env::var(name) {
                    env.insert(name.clone(), value);
                }
            }
            "--env" => {
                i += 1;
                let pair = rest.get(i).ok_or_else(|| anyhow!("--env needs K=V"))?;
                let (k, v) = pair.split_once('=').ok_or_else(|| anyhow!("--env needs K=V"))?;
                env.insert(k.to_string(), v.to_string());
            }
            "--seed" => {
                i += 1;
                seed = rest.get(i).ok_or_else(|| anyhow!("--seed needs a number"))?.parse()?;
            }
            "--" => {
                guest_args.extend(rest[i + 1..].iter().cloned());
                break;
            }
            arg if bundle_path.is_none() && !arg.starts_with("--") => bundle_path = Some(arg.to_string()),
            arg => return Err(anyhow!("unknown argument: {arg}")),
        }
        i += 1;
    }

    let bundle_path = bundle_path.ok_or_else(|| anyhow!("missing bundle path"))?;
    let source = std::fs::read_to_string(&bundle_path)
        .map_err(|e| anyhow!("reading bundle {bundle_path}: {e}"))?;
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;

    let state = HostState::new(root, env, guest_args, seed);
    let guest = Guest::new()?;
    surface::mount(&guest, Rc::clone(&state))?;

    // Boot turn.
    guest.eval(&bundle_path, &source)?;

    // The pump.
    loop {
        if let Some(code) = state.borrow().exit_code {
            return Ok(code);
        }

        let mut batch: Vec<Value> = Vec::new();

        // Facts from fetch threads (non-blocking drain).
        let mut finished: Vec<u32> = Vec::new();
        {
            let st = state.borrow();
            while let Ok(event) = st.fetch_rx.try_recv() {
                match &event {
                    FetchEvent::Done { id } | FetchEvent::Error { id, .. } => finished.push(*id),
                    _ => {}
                }
                batch.push(fetch_event_json(&event));
            }
        }
        {
            let mut st = state.borrow_mut();
            for id in finished {
                st.inflight.remove(&id);
            }
            // Due timers.
            let now = st.monotonic_ms();
            let mut due: Vec<u32> = Vec::new();
            st.timers.retain(|(at, id)| {
                if *at <= now {
                    due.push(*id);
                    false
                } else {
                    true
                }
            });
            due.sort_unstable();
            for id in due {
                batch.push(serde_json::json!({"kind": "timer", "id": id}));
            }
        }

        if !batch.is_empty() {
            let json = serde_json::to_string(&batch).expect("batch is json");
            guest.dispatch(&json)?; // one guest turn, then jobs drain
            continue;
        }

        // Nothing to deliver: sleep until the next possible fact, or exit.
        let (next_timer, inflight) = {
            let st = state.borrow();
            (st.timers.iter().map(|(at, _)| *at).min(), !st.inflight.is_empty())
        };
        if let Some(code) = state.borrow().exit_code {
            return Ok(code);
        }
        if quiescent(&state.borrow()) {
            return Ok(0);
        }
        let now = state.borrow().monotonic_ms();
        let wait_ms = next_timer.map(|at| at.saturating_sub(now)).unwrap_or(60_000).min(60_000);
        if inflight {
            // Block on the fetch channel so streamed chunks wake us promptly.
            let st = state.borrow();
            match st.fetch_rx.recv_timeout(Duration::from_millis(wait_ms.max(1))) {
                Ok(event) => {
                    let is_end = matches!(&event, FetchEvent::Done { .. } | FetchEvent::Error { .. });
                    let id = match &event {
                        FetchEvent::Status { id, .. } | FetchEvent::Chunk { id, .. } |
                        FetchEvent::Done { id } | FetchEvent::Error { id, .. } => *id,
                    };
                    let json = serde_json::to_string(&[fetch_event_json(&event)]).expect("json");
                    drop(st);
                    if is_end {
                        state.borrow_mut().inflight.remove(&id);
                    }
                    guest.dispatch(&json)?;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {}
            }
        } else {
            std::thread::sleep(Duration::from_millis(wait_ms.max(1)));
        }
    }
}
