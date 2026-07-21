// Pocket Pi e2e: bundle guests, run them on the real host binary, assert
// stdout. Covers the headless verification story RUNTIMES.md requires of
// every runtime: timers + fs + env + deterministic RNG (hello), streamed
// fetch with abort against a live SSE server, and the fs sandbox wall.
import { execFile, execFileSync, execSync } from "node:child_process"
import { promisify } from "node:util"
import { createServer } from "node:http"
import { mkdtempSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const root = join(dirname(fileURLToPath(import.meta.url)), "..")
const host = join(root, "host", "target", "debug", "pocket-pi")

let failures = 0
const check = (name, condition, context) => {
  if (condition) {
    console.log(`ok   ${name}`)
  } else {
    failures++
    console.error(`FAIL ${name}\n${context ?? ""}`)
  }
}

execSync("cargo build --manifest-path host/Cargo.toml", { cwd: root, stdio: "inherit" })
execSync("node build/bundle.mjs examples/hello.ts --out dist/hello.js", { cwd: root, stdio: "inherit" })

const work = mkdtempSync(join(tmpdir(), "pocket-pi-e2e-"))
const run = (args, env = {}) =>
  execFileSync(host, args, { encoding: "utf8", env: { ...process.env, ...env } })

// ── 1. hello: timers, fs, deterministic RNG ──────────────────────────────
{
  const a = run(["run", join(root, "dist", "hello.js"), "--root", work, "--seed", "7"])
  const b = run(["run", join(root, "dist", "hello.js"), "--root", work, "--seed", "7"])
  check("hello runs to quiescence", a.includes("skipping fetch; done"), a)
  check("timer order: A before C", a.indexOf("timer A fired") < a.indexOf("timer C fired"), a)
  check("cancelled timer never fires", !a.includes("should never fire"), a)
  check("fs roundtrip", a.includes('content="line-1\\nline-2\\n"'), a)
  check("seeded RNG is deterministic", a.split("\n")[0] === b.split("\n")[0], `${a}\n---\n${b}`)
  const c = run(["run", join(root, "dist", "hello.js"), "--root", work, "--seed", "8"])
  check("different seed, different randoms", c.split("\n")[0] !== a.split("\n")[0], c)
}

// ── 2. streamed fetch + abort against a live SSE server ─────────────────
{
  const server = createServer((req, res) => {
    res.writeHead(200, { "content-type": "text/event-stream" })
    let n = 0
    const timer = setInterval(() => {
      n++
      res.write(`data: tick-${n}\n\n`)
      if (n >= 50) {
        clearInterval(timer)
        res.end()
      }
    }, 15)
    req.on("close", () => clearInterval(timer))
  })
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
  const url = `http://127.0.0.1:${server.address().port}/stream`
  const out = run(["run", join(root, "dist", "hello.js"), "--root", work], { POCKET_PI_URL: "" })
  check("env absent when not allowlisted", out.includes("skipping fetch"), out)
  // async: the SSE server lives in THIS process — a sync exec would block
  // the event loop and starve the stream.
  const { stdout: streamed } = await promisify(execFile)(host, [
    "run", join(root, "dist", "hello.js"), "--root", work, "--env", `POCKET_PI_URL=${url}`
  ], { timeout: 30_000 })
  server.close()
  check("fetch status + content-type", streamed.includes("status=200") && streamed.includes("text/event-stream"), streamed)
  check("SSE chunks stream one at a time", streamed.includes("chunk 3:") && streamed.includes("tick-"), streamed)
  check("abort ends the run (quiescence)", streamed.includes("fetch done after 3 chunks"), streamed)
}

// ── 3. the fs sandbox wall ───────────────────────────────────────────────
{
  const guest = join(work, "escape.js")
  const bundleSrc = `
    console.log("escape read:", JSON.stringify(pi.fsRead("../../../../etc/passwd")))
    console.log("escape abs:", JSON.stringify(pi.fsRead("/etc/passwd")))
    console.log("escape exists:", pi.fsExists("../.."))
    pi.fsWrite("ok.txt", "inside")
    console.log("inside write:", JSON.stringify(pi.fsRead("ok.txt")))
  `
  writeFileSync(guest, bundleSrc)
  const out = run(["run", guest, "--root", join(work, "jail")])
  check("parent traversal is walled", out.includes('escape read: undefined'), out)
  check("absolute paths are walled", out.includes('escape abs: undefined'), out)
  check("escape exists is false", out.includes("escape exists: false"), out)
  check("inside the root works", out.includes('inside write: "inside"'), out)
}

// ── 4. adaptive tick coalescing ──────────────────────────────────────────
{
  const guest = join(work, "tick.js")
  // 8 timers all firing within ~40ms; with a 2 Hz (500ms) window they must
  // collapse into a single dispatched batch. __pi_dispatch counts turns.
  writeFileSync(guest, `
    pi.tickHz(2)
    let turns = 0, facts = 0
    const base = globalThis.__pi_dispatch
    globalThis.__pi_dispatch = (j) => { turns++; facts += JSON.parse(j).length; base && 0 }
    for (let i = 1; i <= 8; i++) pi.timerStart(i, 5 + i * 4)
    // report after everything has drained, then exit
    pi.timerStart(99, 900)
    const seen = new Set()
    globalThis.__pi_dispatch = (j) => {
      turns++
      const batch = JSON.parse(j)
      facts += batch.length
      for (const e of batch) seen.add(e.id)
      if (seen.has(99)) { console.log("turns=" + turns + " facts=" + facts); pi.exit(0) }
    }
  `)
  const out = run(["run", guest, "--root", work])
  const m = out.match(/turns=(\d+) facts=(\d+)/)
  check("tick: all 8 timers delivered", m && Number(m[2]) >= 8, out)
  check("tick: 2 Hz coalesces bursts into few turns", m && Number(m[1]) <= 3, out)

  // control: no tickHz → event-driven, more turns for the same burst
  const guest2 = join(work, "notick.js")
  writeFileSync(guest2, `
    let turns = 0
    globalThis.__pi_dispatch = (j) => { turns++; const b=JSON.parse(j);
      if (b.some(e=>e.id===99)) { console.log("turns="+turns); pi.exit(0) } }
    for (let i = 1; i <= 8; i++) pi.timerStart(i, 5 + i * 12)
    pi.timerStart(99, 700)
  `)
  const out2 = run(["run", guest2, "--root", work])
  const m2 = out2.match(/turns=(\d+)/)
  check("no-tick: spread timers wake more turns than coalesced", m2 && Number(m2[1]) >= 4, out2)
}

if (failures > 0) {
  console.error(`\n${failures} failure(s)`)
  process.exit(1)
}
console.log("\nall e2e checks passed")
