/**
 * Pocket Pi SDK prelude — the guest half of the `pi` surface.
 *
 * Bundled BEFORE the product by build/bundle.mjs. Adapts the spec-pinned
 * surface (spec/spec.ts) to the platform APIs the pi ecosystem expects:
 * timers, fetch with streamed bodies + AbortSignal, TextEncoder/Decoder,
 * URL, performance, crypto.getRandomValues. State mirrors live here (Law
 * 1); the host owns time and I/O and delivers facts to __pi_dispatch in
 * per-tick batches (Law 3).
 */

/* global pi */
;(() => {
  "use strict"
  if (typeof pi === "undefined") {
    throw new Error("pocket-pi prelude: the `pi` surface is not mounted")
  }

  // ── timers ────────────────────────────────────────────────────────────
  const timers = new Map() // id -> {fn, args, every: number|null}
  let nextTimerId = 1

  globalThis.setTimeout = (fn, ms, ...args) => {
    const id = nextTimerId++
    timers.set(id, { fn, args, every: null })
    pi.timerStart(id, Number(ms) || 0)
    return id
  }
  globalThis.clearTimeout = (id) => {
    if (timers.delete(id)) pi.timerClear(id)
  }
  globalThis.setInterval = (fn, ms, ...args) => {
    const id = nextTimerId++
    timers.set(id, { fn, args, every: Math.max(1, Number(ms) || 1) })
    pi.timerStart(id, Math.max(1, Number(ms) || 1))
    return id
  }
  globalThis.clearInterval = globalThis.clearTimeout

  if (typeof globalThis.queueMicrotask !== "function") {
    globalThis.queueMicrotask = (fn) => {
      Promise.resolve().then(fn)
    }
  }

  if (typeof globalThis.performance === "undefined") {
    globalThis.performance = { now: () => pi.monotonic(), timeOrigin: pi.now() }
  }

  // Host RNG: seedable, so `--seed` gives reproducible agent runs.
  Math.random = () => pi.random()

  if (typeof globalThis.crypto === "undefined") globalThis.crypto = {}
  if (typeof globalThis.crypto.getRandomValues !== "function") {
    globalThis.crypto.getRandomValues = (array) => {
      for (let i = 0; i < array.length; i++) array[i] = Math.floor(pi.random() * 256)
      return array
    }
  }

  // ── text encoding ─────────────────────────────────────────────────────
  if (typeof globalThis.TextEncoder === "undefined") {
    globalThis.TextEncoder = class TextEncoder {
      encode(text) {
        const out = []
        for (const ch of String(text)) {
          let cp = ch.codePointAt(0)
          if (cp < 0x80) out.push(cp)
          else if (cp < 0x800) out.push(0xc0 | (cp >> 6), 0x80 | (cp & 0x3f))
          else if (cp < 0x10000)
            out.push(0xe0 | (cp >> 12), 0x80 | ((cp >> 6) & 0x3f), 0x80 | (cp & 0x3f))
          else
            out.push(
              0xf0 | (cp >> 18),
              0x80 | ((cp >> 12) & 0x3f),
              0x80 | ((cp >> 6) & 0x3f),
              0x80 | (cp & 0x3f)
            )
        }
        return new Uint8Array(out)
      }
    }
  }
  if (typeof globalThis.TextDecoder === "undefined") {
    globalThis.TextDecoder = class TextDecoder {
      decode(bytes) {
        if (bytes === undefined) return ""
        const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)
        let out = ""
        let i = 0
        while (i < arr.length) {
          const b = arr[i]
          let cp, extra
          if (b < 0x80) { cp = b; extra = 0 }
          else if (b < 0xe0) { cp = b & 0x1f; extra = 1 }
          else if (b < 0xf0) { cp = b & 0x0f; extra = 2 }
          else { cp = b & 0x07; extra = 3 }
          i++
          while (extra-- > 0 && i < arr.length) { cp = (cp << 6) | (arr[i++] & 0x3f) }
          out += String.fromCodePoint(cp)
        }
        return out
      }
    }
  }

  // ── URL ───────────────────────────────────────────────────────────────
  if (typeof globalThis.URL === "undefined") {
    globalThis.URL = class URL {
      constructor(href, base) {
        const raw = base !== undefined && !/^[a-z][a-z0-9+.-]*:/i.test(href)
          ? String(base).replace(/\/[^/]*$/, "/") + String(href).replace(/^\.?\//, "")
          : String(href)
        const m = /^([a-z][a-z0-9+.-]*):\/\/([^/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(raw)
        if (m === null) throw new TypeError(`Invalid URL: ${raw}`)
        this.protocol = m[1].toLowerCase() + ":"
        this.host = m[2]
        const at = m[2].lastIndexOf(":")
        this.hostname = at >= 0 ? m[2].slice(0, at) : m[2]
        this.port = at >= 0 ? m[2].slice(at + 1) : ""
        this.pathname = m[3] === "" ? "/" : m[3]
        this.search = m[4] ?? ""
        this.hash = m[5] ?? ""
        this.origin = `${this.protocol}//${this.host}`
        this.href = `${this.origin}${this.pathname}${this.search}${this.hash}`
      }
      toString() { return this.href }
      toJSON() { return this.href }
    }
  }
  if (typeof globalThis.URLSearchParams === "undefined") {
    globalThis.URLSearchParams = class URLSearchParams {
      constructor(init) {
        this._pairs = []
        if (typeof init === "string") {
          for (const part of init.replace(/^\?/, "").split("&")) {
            if (part === "") continue
            const eq = part.indexOf("=")
            const k = eq < 0 ? part : part.slice(0, eq)
            const v = eq < 0 ? "" : part.slice(eq + 1)
            this._pairs.push([decodeURIComponent(k), decodeURIComponent(v)])
          }
        }
      }
      append(k, v) { this._pairs.push([String(k), String(v)]) }
      get(k) { const hit = this._pairs.find((p) => p[0] === k); return hit ? hit[1] : null }
      set(k, v) { this._pairs = this._pairs.filter((p) => p[0] !== k); this.append(k, v) }
      toString() {
        return this._pairs.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join("&")
      }
      *[Symbol.iterator]() { yield* this._pairs }
    }
  }

  // ── AbortController ───────────────────────────────────────────────────
  if (typeof globalThis.AbortController === "undefined") {
    class AbortSignalShim {
      constructor() {
        this.aborted = false
        this.reason = undefined
        this._listeners = new Set()
        this.onabort = null
      }
      addEventListener(type, fn) { if (type === "abort") this._listeners.add(fn) }
      removeEventListener(type, fn) { if (type === "abort") this._listeners.delete(fn) }
      throwIfAborted() { if (this.aborted) throw this.reason }
      _fire() {
        const event = { type: "abort", target: this }
        if (typeof this.onabort === "function") this.onabort(event)
        for (const fn of [...this._listeners]) fn(event)
      }
    }
    globalThis.AbortSignal = AbortSignalShim
    globalThis.AbortController = class AbortController {
      constructor() { this.signal = new AbortSignalShim() }
      abort(reason) {
        if (this.signal.aborted) return
        this.signal.aborted = true
        this.signal.reason = reason ?? Object.assign(new Error("This operation was aborted"), { name: "AbortError" })
        this.signal._fire()
      }
    }
  }

  // ── fetch over the pi surface ─────────────────────────────────────────
  // Streamed text chunks become Uint8Array body chunks; body implements
  // getReader() and async iteration — the two shapes SSE consumers use.
  const inflight = new Map() // id -> request state
  let nextFetchId = 1
  const encoder = new TextEncoder()

  class BodyStream {
    constructor(state) {
      this._state = state
      this.locked = false
    }
    getReader() {
      this.locked = true
      const state = this._state
      return {
        read: () => state._read(),
        cancel: (reason) => state._cancel(reason),
        releaseLock: () => { this.locked = false }
      }
    }
    cancel(reason) { return this._state._cancel(reason) }
    [Symbol.asyncIterator]() {
      const state = this._state
      return {
        next: async () => {
          const r = await state._read()
          return r.done ? { done: true, value: undefined } : { done: false, value: r.value }
        },
        return: async () => {
          await state._cancel("iterator returned")
          return { done: true, value: undefined }
        }
      }
    }
  }

  class FetchState {
    constructor(id, url) {
      this.id = id
      this.url = url
      this.chunks = []       // queued Uint8Array
      this.done = false
      this.error = null
      this.waiters = []      // pending read() resolvers
      this.headResolve = null
      this.headReject = null
    }
    _push() {
      while (this.waiters.length > 0) {
        if (this.chunks.length > 0) {
          this.waiters.shift()({ done: false, value: this.chunks.shift() })
        } else if (this.error !== null) {
          const w = this.waiters.shift()
          w.reject ? w.reject(this.error) : w({ done: true, value: undefined })
          // readers get the error via rejected read
        } else if (this.done) {
          this.waiters.shift()({ done: true, value: undefined })
        } else {
          break
        }
      }
    }
    _read() {
      return new Promise((resolve, reject) => {
        if (this.chunks.length > 0) return resolve({ done: false, value: this.chunks.shift() })
        if (this.error !== null) return reject(this.error)
        if (this.done) return resolve({ done: true, value: undefined })
        const waiter = (r) => resolve(r)
        waiter.reject = reject
        this.waiters.push(waiter)
      })
    }
    _cancel(_reason) {
      if (!this.done && this.error === null) pi.fetchAbort(this.id)
      return Promise.resolve()
    }
  }

  globalThis.fetch = (input, init = {}) => {
    const url = typeof input === "string" ? input : input.url ?? String(input)
    const id = nextFetchId++
    const state = new FetchState(id, url)
    inflight.set(id, state)

    const headers = {}
    const rawHeaders = init.headers ?? {}
    if (rawHeaders && typeof rawHeaders.forEach === "function" && !Array.isArray(rawHeaders)) {
      rawHeaders.forEach((v, k) => { headers[String(k)] = String(v) })
    } else if (Array.isArray(rawHeaders)) {
      for (const [k, v] of rawHeaders) headers[String(k)] = String(v)
    } else {
      for (const k of Object.keys(rawHeaders)) headers[k] = String(rawHeaders[k])
    }

    const signal = init.signal
    if (signal !== undefined && signal !== null) {
      if (signal.aborted) {
        inflight.delete(id)
        return Promise.reject(Object.assign(new Error("This operation was aborted"), { name: "AbortError" }))
      }
      signal.addEventListener("abort", () => pi.fetchAbort(id))
    }

    return new Promise((resolve, reject) => {
      state.headResolve = (status, headerMap) => {
        const lower = {}
        for (const k of Object.keys(headerMap)) lower[k.toLowerCase()] = headerMap[k]
        resolve({
          ok: status >= 200 && status < 300,
          status,
          statusText: String(status),
          url,
          headers: {
            get: (name) => lower[String(name).toLowerCase()] ?? null,
            has: (name) => String(name).toLowerCase() in lower,
            forEach: (fn) => { for (const k of Object.keys(lower)) fn(lower[k], k) }
          },
          body: new BodyStream(state),
          text: async () => {
            const decoder = new TextDecoder()
            let out = ""
            for (;;) {
              const r = await state._read()
              if (r.done) return out
              out += decoder.decode(r.value)
            }
          },
          json: async function () { return JSON.parse(await this.text()) },
          arrayBuffer: async () => {
            const parts = []
            for (;;) {
              const r = await state._read()
              if (r.done) break
              parts.push(r.value)
            }
            const total = parts.reduce((n, p) => n + p.length, 0)
            const out = new Uint8Array(total)
            let offset = 0
            for (const p of parts) { out.set(p, offset); offset += p.length }
            return out.buffer
          }
        })
      }
      state.headReject = reject
      pi.fetchStart(id, JSON.stringify({
        url,
        method: init.method ?? "GET",
        headers,
        ...(init.body !== undefined && init.body !== null ? { body: String(init.body) } : {})
      }))
    })
  }

  // ── the guest turn: host-delivered fact batches ───────────────────────
  globalThis.__pi_dispatch = (batchJson) => {
    const batch = JSON.parse(batchJson)
    for (const event of batch) {
      switch (event.kind) {
        case "timer": {
          const t = timers.get(event.id)
          if (t === undefined) break
          if (t.every === null) timers.delete(event.id)
          else pi.timerStart(event.id, t.every)
          try {
            t.fn(...t.args)
          } catch (e) {
            console.error("pocket-pi: timer callback threw:", e && e.stack ? e.stack : e)
          }
          break
        }
        case "fetchStatus": {
          const s = inflight.get(event.id)
          if (s && s.headResolve) {
            const resolve = s.headResolve
            s.headResolve = null
            s.headReject = null
            resolve(event.status, event.headers ?? {})
          }
          break
        }
        case "fetchChunk": {
          const s = inflight.get(event.id)
          if (s) { s.chunks.push(encoder.encode(event.text)); s._push() }
          break
        }
        case "fetchDone": {
          const s = inflight.get(event.id)
          if (s) { s.done = true; s._push(); inflight.delete(event.id) }
          break
        }
        case "fetchError": {
          const s = inflight.get(event.id)
          if (s) {
            const error = Object.assign(new Error(event.message), event.aborted ? { name: "AbortError" } : {})
            if (s.headReject !== null) {
              // head never arrived → the fetch() promise itself rejects
              s.headReject(error)
              s.headResolve = null
              s.headReject = null
            }
            s.error = error
            s._push()
            inflight.delete(event.id)
          }
          break
        }
        default:
          break
      }
    }
  }
})();
