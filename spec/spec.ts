/**
 * The `pi` surface — Pocket Pi's entire guest↔host boundary, pinned as data.
 *
 * Pocket Pi is a member of the Pocket runtime family (see PocketJS
 * RUNTIMES.md): Runtime = ⟨Cores, Surfaces, Guest⟩. The domain here is the
 * agent loop — pi-class LLM streaming and event-sourced agent worlds — so
 * the core owns exactly what an agent runtime must not leave to the guest:
 * the clock, timers, HTTP streaming, a sandboxed file root, and the
 * environment. The guest is one QuickJS realm evaluating one bundled
 * program (e.g. the cat agent framework); the SDK adapts this surface to
 * the web-ish API the pi ecosystem expects (fetch, AbortSignal, timers,
 * node:fs for the event log).
 *
 * The three laws (RUNTIMES.md §3) hold in agent-domain form:
 *   Law 1 — state lives in the core: sockets, timer wheel, file handles.
 *   Law 2 — intent crosses as ops (sync, enumerable), facts cross as
 *           events (per-tick batches). Nothing else crosses.
 *   Law 3 — one guest turn per host tick. The host pumps: deliver the
 *           event batch to `globalThis.__pi_dispatch`, then drain the job
 *           queue. The guest never owns a timer or a thread. A run EXITS
 *           when the world is quiescent: no due timers, no inflight
 *           requests, no pending jobs.
 *
 * Capability = surface: no ambient network, filesystem, or process access.
 * fs ops are confined to a host-chosen root; env reads are allowlisted at
 * launch. Codes are append-only — never renumbered, never reused.
 */

export interface OpSpec {
  readonly code: number
  readonly name: string
  /** Guest-visible signature (documentation; ops are plain functions). */
  readonly sig: string
  readonly doc: string
}

export interface EventSpec {
  readonly code: number
  readonly name: string
  /** Fields of the batch entry, beyond `kind`. */
  readonly fields: string
  readonly doc: string
}

export const SURFACE = "pi" as const
export const VERSION = 1 as const

export const OPS: readonly OpSpec[] = [
  { code: 1, name: "now", sig: "() => number", doc: "Wall clock, ms since epoch. Host-owned so deterministic hosts can fix it." },
  { code: 2, name: "monotonic", sig: "() => number", doc: "Monotonic ms since host start." },
  { code: 3, name: "random", sig: "() => number", doc: "Host RNG in [0,1). Seedable for deterministic replays." },
  { code: 4, name: "timerStart", sig: "(id: number, ms: number) => void", doc: "Arm a one-shot timer; fires as a `timer` event. Guests never own timers (Law 3)." },
  { code: 5, name: "timerClear", sig: "(id: number) => void", doc: "Disarm a timer; already-fired ids are ignored." },
  { code: 6, name: "fetchStart", sig: "(id: number, req: string) => void", doc: "Begin an HTTP request. `req` is JSON {url, method?, headers?, body?}. Progress arrives as fetch* events; response bodies stream as text chunks (SSE-friendly; v1 is text-only)." },
  { code: 7, name: "fetchAbort", sig: "(id: number) => void", doc: "Abort an inflight request; a fetchError event with aborted=true follows." },
  { code: 8, name: "fsRead", sig: "(path: string) => string | null", doc: "Read a UTF-8 file under the sandbox root; null when absent." },
  { code: 9, name: "fsWrite", sig: "(path: string, text: string) => void", doc: "Write (create/truncate) a UTF-8 file under the root, creating parents." },
  { code: 10, name: "fsAppend", sig: "(path: string, text: string) => void", doc: "Append UTF-8 text under the root, creating parents. The event-log primitive." },
  { code: 11, name: "fsExists", sig: "(path: string) => boolean", doc: "Whether a path exists under the root." },
  { code: 12, name: "fsRemove", sig: "(path: string) => void", doc: "Remove a file under the root; missing is a no-op." },
  { code: 13, name: "env", sig: "(name: string) => string | null", doc: "Read an allowlisted environment value (--allow-env / --env at launch)." },
  { code: 14, name: "args", sig: "() => string", doc: "JSON array of guest argv (everything after the bundle path)." },
  { code: 15, name: "exit", sig: "(code: number) => void", doc: "Request host exit with a status code at the end of this turn." }
] as const

export const EVENTS: readonly EventSpec[] = [
  { code: 1, name: "timer", fields: "{id: number}", doc: "An armed timer elapsed." },
  { code: 2, name: "fetchStatus", fields: "{id: number, status: number, headers: Record<string,string>}", doc: "Response head arrived." },
  { code: 3, name: "fetchChunk", fields: "{id: number, text: string}", doc: "A body chunk (UTF-8 text, lossy on invalid bytes; v1 is text-only)." },
  { code: 4, name: "fetchDone", fields: "{id: number}", doc: "Body finished cleanly." },
  { code: 5, name: "fetchError", fields: "{id: number, message: string, aborted: boolean}", doc: "Request failed or was aborted." }
] as const

/** The guest entry point the host dispatches event batches to. */
export const DISPATCH = "__pi_dispatch" as const
