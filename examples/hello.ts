// hello.ts — exercises every pillar of the pi surface without any deps:
// timers, the fs sandbox, env, args, Math.random determinism, and (when
// POCKET_PI_URL is set) a streamed fetch with abort.
//
//   node build/bundle.mjs examples/hello.ts --out dist/hello.js
//   host/target/release/pocket-pi run dist/hello.js --root .runs --seed 7

declare const pi: {
  env: (name: string) => string | null
  args: () => string
  monotonic: () => number
}

import { appendFileSync, existsSync, readFileSync, rmSync } from "node:fs"

const log = (line: string) => console.log(line)

log(`hello from QuickJS · args=${pi.args()} · seed-random=${Math.random().toFixed(6)}`)

rmSync("hello.log")
appendFileSync("hello.log", "line-1\n")
appendFileSync("hello.log", "line-2\n")
log(`fs: exists=${existsSync("hello.log")} content=${JSON.stringify(readFileSync("hello.log", "utf8"))}`)

const t0 = pi.monotonic()
setTimeout(() => {
  log(`timer A fired at ~${Math.round(pi.monotonic() - t0)}ms`)
}, 20)
const cancelled = setTimeout(() => log("timer B should never fire"), 30)
clearTimeout(cancelled)
setTimeout(async () => {
  log("timer C fired (order check: after A)")

  const url = pi.env("POCKET_PI_URL")
  if (url == null) {
    log("no POCKET_PI_URL — skipping fetch; done")
    return
  }
  const controller = new AbortController()
  const response = await fetch(url, { signal: controller.signal })
  log(`fetch: status=${response.status} content-type=${response.headers.get("content-type")}`)
  const decoder = new TextDecoder()
  let chunks = 0
  for await (const chunk of response.body as AsyncIterable<Uint8Array>) {
    chunks++
    log(`chunk ${chunks}: ${JSON.stringify(decoder.decode(chunk)).slice(0, 60)}`)
    if (chunks >= 3) {
      controller.abort()
      break
    }
  }
  log(`fetch done after ${chunks} chunks (aborted stream)`)
}, 40)
