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

## Layout

```
spec/       spec.ts (the pi surface, as data) + codegen.mjs (→ Rust, drift-guarded)
host/       Rust core: guest.rs (realm hosting, pocket-mod pattern),
            surface.rs (ops + fetch threads + fs jail), main.rs (CLI + pump)
sdk/        prelude.js (fetch/streams/timers/URL/encoding/abort over the surface),
            node-fs.js / node-path.js / node-crypto.js (bundler aliases)
build/      bundle.mjs — entry.ts → one iife guest bundle, prelude first (node + esbuild)
examples/   hello.ts — timers, fs, env, seeded RNG, streamed fetch + abort
test/       e2e.mjs — hello goldens, live SSE stream + abort, sandbox walls
```

## Fidelity notes

- Response bodies stream as UTF-8 text chunks (SSE-first; v1 is text-only —
  a binary event is an append away when a product needs it).
- `fetchAbort` is honored at chunk boundaries; a stream that goes silent
  holds its thread until the next byte or connection close.
- The realm is one context; module loading is bundled-ahead (no dynamic
  import), matching the PocketJS product-bundle model.
