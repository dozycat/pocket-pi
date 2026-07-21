# Pocket Pi

A member of the [Pocket runtime family](https://github.com/pocket-stack/pocketjs)
for the **agent-loop domain**: one QuickJS realm running a pi-class agent
program (LLM streaming, tool loops, event-sourced agent worlds), with a tiny
Rust core owning everything a guest must never own — the clock, timers, HTTP
streaming, a sandboxed file root, the environment, and the RNG.

Built to PocketJS's RUNTIMES.md discipline: `Runtime = ⟨Core, Surface, Guest⟩`.
The vocabulary here is not nodes and styles — it is **requests, chunks,
timers, and an append-only log**. No bun anywhere: the toolchain is Rust +
node + esbuild, and the product is one static binary plus one JS bundle.

```
┌──────────────────────────────────────────────────────┐
│ guest (QuickJS)     one bundled agent program        │
│   e.g. cat: Effect fibers, event kernel, agent graph │
│   SDK prelude: fetch/AbortSignal/timers/node:fs …    │
├──────────────── the `pi` surface (spec/) ────────────┤
│   ops:    now monotonic random timerStart timerClear │
│           fetchStart fetchAbort fs* env args exit    │
│   events: timer fetchStatus fetchChunk fetchDone     │
│           fetchError            (per-tick batches)   │
├──────────────────────────────────────────────────────┤
│ core (Rust)   clock · timer wheel · ureq streaming   │
│               threads · fs sandbox · env allowlist   │
│               · seeded RNG · quiescence-driven pump  │
└──────────────────────────────────────────────────────┘
```

## The laws, agent-domain form

1. **State lives in the core; guests hold mirrors.** Sockets, timers, and
   file handles never cross the boundary — the SDK keeps JS-side mirrors
   (pending fetches, timer callbacks) updated from event batches.
2. **Intent crosses as ops, facts cross as events, both spec-pinned.**
   `spec/spec.ts` is the single source of truth; `spec/codegen.mjs`
   generates the Rust side and CI byte-compares it (the drift guard).
   Codes are append-only.
3. **One guest turn per host tick.** The host pump collects facts (due
   timers, fetch progress) into one batch, delivers it as one
   `__pi_dispatch` call, and drains the job queue. The guest never owns a
   timer or thread — which is why a run **exits at quiescence**: no due
   timers, no inflight requests, nothing pending. Headless agent runs are
   deterministic by construction (`--seed` pins Math.random).

Capability = surface: no ambient filesystem (fs ops are jailed under
`--root`), no ambient network beyond `fetchStart`, env is allowlisted at
launch (`--allow-env NAME` / `--env K=V`).

## Quickstart

```sh
npm install                        # esbuild (the only JS dependency)
npm run build:host                 # cargo build --release
node build/bundle.mjs examples/hello.ts --out dist/hello.js
host/target/release/pocket-pi run dist/hello.js --root .runs --seed 7
npm test                           # spec drift guard + 14 e2e checks
```

## Running cat (the reference product)

[cat](https://github.com/paperboytm/cat) — an Effect-based agent framework
with an event kernel and a self-modifying agent graph — runs unmodified:

```sh
node build/bundle.mjs ../cat/examples/paperboy.ts \
  --out dist/paperboy.js --modules ../cat/node_modules
host/target/release/pocket-pi run dist/paperboy.js --root .runs/paperboy
```

That demo boots 16 agents, runs delegation cascades, lets an agent rewrite
its own persona/skills and spawn a subagent, persists every event to JSONL
through the fs sandbox, and proves replay equality on reopen — all inside
QuickJS.

## Pocket Cat — the native macOS desktop pet

`stage/` is a second member of the runtime family that mounts a **`stage`
surface** instead of `pi`: a native macOS window (via `minifb` — no
Electron) with a software framebuffer, driven by a QuickJS guest
(`cat-brain.js`). A pixel cat sits by a pixel CRT that mirrors, at a low
frame rate, **what the agent observes**; when the mirror shows something
private the cat **turns away and covers its face**, and the screen
censors. Right-click is a menu (observe on/off, privacy, browser-use,
nap); the cat reacts, naps, and can be petted.

The split is RUNTIMES.md's three laws in the widget domain: the Rust core
owns per-frame work (window, framebuffer, clock, input, scene rotation);
the QuickJS guest owns policy (reactions, the privacy judgement,
commands). Cadence is adaptive — the cat idles at ~2 fps and bursts to
~14 fps while reacting (the same coalescing idea as `pi.tickHz`).

```sh
# the sprite art is the user's (dozycat-io/design), not committed here:
node stage/gen-assets.mjs --design ../dozycat-io/design   # or `npm run gen:cat`
npm run cat                     # open the widget window
npm run cat:capture             # headless: render 5 key states to stage/captures/*.png
```

The headless `--capture` mode renders the watch / privacy-avert /
browser-use / menu / nap / chat states straight to PNG (no display
needed) — the runtime's verification story, exactly as RUNTIMES.md
requires.

**Really connected (macOS):**
- **OS screencapture** — the monitor mirrors the actual desktop: a thread
  runs the system `screencapture` + `sips` at ~1.4 fps, decoded and blitted
  live (needs Screen Recording permission; a blank frame shows a GRANT
  hint). The only permission required.
- **Activity sequences** — every frontmost-app change (via `NSWorkspace`,
  no permission) appends a real record to `~/.pocket-cat/sequences.jsonl`,
  and drives privacy-avert against a sensitive-app list.
- **@cat in-context chat** — right-click → CHAT opens a native input
  (IME/CJK), and the message + frontmost app + recent sequences POST to
  `cat-swarm`: one graph script that boots the **whole cat agent swarm**
  (cat-chat, gardener, sequences, workflow-classifier/narrator, …) live
  on the harness — the pi turn loop against a real model (openai-codex via
  pi-ai), the world persisted to a JSONL event log (garden/workflows/chat
  replay on restart). Not santi. The reply renders in a fontdue panel above
  the cat (any language). Start:

  ```sh
  # in the cat repo:
  npx tsx examples/cat-swarm.ts     # the swarm on :8848 — /chat /observe /synthesize /garden
  ```

## Layout

```
spec/       spec.ts (the pi surface, as data) + codegen.mjs (→ Rust, drift-guarded)
host/       Rust core: guest.rs (realm hosting, pocket-mod pattern),
            surface.rs (ops + fetch threads + fs jail), main.rs (CLI + pump)
sdk/        prelude.js (fetch/streams/timers/URL/encoding/abort over the surface),
            node-fs.js / node-path.js / node-crypto.js (bundler aliases)
build/      bundle.mjs — entry.ts → one iife guest bundle, prelude first (node + esbuild)
examples/   hello.ts — timers, fs, env, seeded RNG, streamed fetch + abort
test/       e2e.mjs — hello goldens, live SSE stream + abort, sandbox walls, tick coalescing
stage/      pocket-cat — native macOS pixel desktop pet (the `stage` surface):
            fb.rs (framebuffer + 5×7 font), sprites.rs (PNG decode), cat-brain.js
            (QuickJS policy), main.rs (window host + headless capture);
            gen-assets.mjs packs the user's sprites (not committed)
```

## Fidelity notes

- Response bodies stream as UTF-8 text chunks (SSE-first; v1 is text-only —
  a binary event is an append away when a product needs it).
- `fetchAbort` is honored at chunk boundaries; a stream that goes silent
  holds its thread until the next byte or connection close.
- The realm is one context; module loading is bundled-ahead (no dynamic
  import), matching the PocketJS product-bundle model.
