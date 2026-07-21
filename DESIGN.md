# Pocket Pi — design

## Why a runtime, not a node script

The PocketJS thesis (RUNTIMES.md): the unit of creation is a script acting
on a small, closed, spec-pinned vocabulary — and each domain deserves its
own deliberately small engine rather than a general-purpose one. Agent
loops are such a domain. What an agent program actually needs from a
platform is tiny and enumerable:

- a clock and timers (Effect schedulers, idle timeouts, backoff),
- streamed HTTP with cancellation (the LLM provider boundary — pi's SSE),
- an append-only durable log (event-sourced worlds),
- environment secrets, an RNG, a console.

Everything else — fibers, kernels, projections, agent graphs — is guest
code. Pinning those five things as the `pi` surface buys the same
properties the UI runtime bought on the PSP: sandboxing that falls out of
the ontology instead of policy, byte-exact headless verification, and
hosts that can be reimplemented (desktop binary today; the same surface
could mount in pocketjs-psp or WASM tomorrow) without touching products.

## The pump (Law 3 without a frame rate)

UI runtimes tick at 60 fps. An agent runtime's natural tick is **"when
there are facts to deliver"**:

```
eval bundle                     # the boot turn
loop:
  batch ← drained fetch events + due timers
  if batch ≠ ∅:  __pi_dispatch(batch); drain jobs; continue
  if exit requested: exit(code)
  if no timers ∧ no inflight:   exit(0)        # quiescence
  else block on fetch channel (bounded by next timer deadline)
```

Quiescence-driven exit is the agent-domain analogue of "frame content is a
pure function of tick index + inputs": a headless run ends exactly when
the world can never change again, so CI assertions never race and never
hang. `--seed` pins the surface RNG (`Math.random` is rebound in the
prelude), making whole agent runs reproducible.

## The surface (what crosses, and why so little)

Ops are synchronous Rust closures (rquickjs `Function`s) — intent only.
Facts return exclusively through per-tick JSON batches to
`globalThis.__pi_dispatch`. Two deliberate consequences:

- **fs ops are sync.** The event-sourcing pattern (cat's JSONL store)
  requires a committed event to be durable before the commit pipeline
  continues; a sync `fsAppend` under a jailed root is exactly that
  primitive, with none of the async-fs ceremony.
- **fetch is split** into `fetchStart`/`fetchAbort` ops and
  status/chunk/done/error events. The SDK rebuilds the whole web shape —
  `fetch()`, `Response`, a ReadableStream-lite with `getReader()` and
  async iteration, `AbortSignal` wiring — as guest-side mirrors (Law 1).
  pi-class SSE consumers run unchanged.

HTTP lives on one thread per request (blocking `ureq` + rustls, no tokio),
funneled through one mpsc channel into the pump. Abort is a relaxed
`AtomicBool` honored at chunk boundaries — cheap, and correct for SSE
where silence between events is legal (a read-timeout would kill legit
streams; see the fidelity notes in README).

## The SDK preludes (guest mirrors)

`sdk/prelude.js` installs, in dependency order: timers over
`timerStart`/timer events; `queueMicrotask`/`performance`/`crypto`
fallbacks; UTF-8 `TextEncoder`/`TextDecoder`; a minimal `URL` (Effect
hashes URLs at module init); `AbortController`; and `fetch`. The bundler
prepends it, ending with an explicit `;` — two concatenated iifes
otherwise ASI-chain into one call expression.

`node:fs`/`node:path`/`node:crypto` are esbuild **aliases**, not globals:
guests that never import them carry nothing, and the fs alias is the only
path to the (already jailed) fs ops. `node:crypto` ships a real SHA-256 —
guests use it for content-addressed cache keys, so stability matters more
than speed at agent scale.

## What was proven

`test/e2e.mjs` (14 checks): timer ordering + cancellation, fs roundtrip,
seed determinism (same seed ⇒ same run; different seed ⇒ different run),
env allowlisting, live SSE streaming chunk-at-a-time, abort-to-quiescence,
and the sandbox walls (parent traversal, absolute paths).

The reference product is [cat](https://github.com/paperboytm/cat): its
paperboy demo — 16 agents on an Effect-based event kernel with a
self-modifying agent graph — runs unmodified on this host, writes its
event log through the jail, and passes its own replay-equality check
inside QuickJS.

## Cuts (restorable, append-only)

- **Binary fetch bodies** — add a `fetchChunkB64` event when a product
  needs one; text-only keeps v1 honest about what SSE needs.
- **Concurrent realms / worker pools** — one realm per process; agent
  parallelism belongs to the product (cat's kernel is single-writer by
  design anyway).
- **Dynamic import / module loader** — products are bundled ahead, the
  PocketJS way.
- **A `ui` mount** — nothing prevents a future host from mounting both
  `pi` and `ui` in one realm (an agent with a PocketJS face); the
  mechanism is pocket-mod's, unchanged.
