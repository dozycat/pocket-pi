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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

use guest::Guest;
use surface::{quiescent, HostState};

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

    // The pump. Facts accumulate in `pending` and are delivered as ONE guest
    // turn per wake; a nonzero coalesce window (set by the guest via tickHz)
    // caps how often the guest is woken without ever imposing a floor — an
    // idle run still sleeps to the next real event and exits at quiescence.
    let mut pending: Vec<Value> = Vec::new();
    let mut last_dispatch: u64 = 0;
    loop {
        if let Some(code) = state.borrow().exit_code {
            return Ok(code);
        }

        // 1) collect any facts available right now.
        pending.append(&mut state.borrow_mut().collect_facts());

        // 2) something to deliver?
        if !pending.is_empty() {
            let (coalesce, now) = {
                let st = state.borrow();
                (st.coalesce_ms, st.monotonic_ms())
            };
            let since = now.saturating_sub(last_dispatch);
            if coalesce > 0 && since < coalesce {
                // Within the coalescing window: wait the remainder, merging any
                // facts that arrive, then loop back to dispatch them together.
                wait_for_facts(&state, coalesce - since)?;
                continue;
            }
            let json = serde_json::to_string(&pending).expect("batch is json");
            pending.clear();
            last_dispatch = state.borrow().monotonic_ms();
            guest.dispatch(&json)?; // one guest turn, then jobs drain
            continue;
        }

        // 3) nothing pending: exit at quiescence, else sleep to the next fact.
        if quiescent(&state.borrow()) {
            return Ok(0);
        }
        let wait_ms = state.borrow().next_timer_in().unwrap_or(60_000).min(60_000);
        wait_for_facts(&state, wait_ms)?;
    }
}

/// Sleep up to `ms`, but wake early if a fetch thread reports (so streamed
/// chunks stay responsive). Received events are queued for the next collect.
fn wait_for_facts(state: &Rc<RefCell<HostState>>, ms: u64) -> Result<()> {
    let ms = ms.max(1);
    let inflight = !state.borrow().inflight.is_empty();
    if inflight {
        // recv_timeout borrows the receiver; requeue via the state's own sender
        // is unnecessary — collect_facts drains the same channel next loop, so
        // just block here and let the next iteration pick everything up.
        let deadline = Duration::from_millis(ms);
        let st = state.borrow();
        match st.fetch_rx.recv_timeout(deadline) {
            Ok(event) => {
                // Put it back so collect_facts sees it uniformly.
                st.fetch_tx_clone().send(event).ok();
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {}
        }
    } else {
        std::thread::sleep(Duration::from_millis(ms));
    }
    Ok(())
}
