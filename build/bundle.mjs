#!/usr/bin/env node
// Pocket Pi bundler — entry.ts -> one iife guest bundle, prelude first.
// Node + esbuild only (the Pocket Pi toolchain deliberately has no bun
// dependency). Node builtins used by guests (node:fs, node:path) are
// aliased to pi-surface shims; everything else must be plain JS/TS.
//
//   node build/bundle.mjs <entry.ts> --out dist/app.js [--modules DIR]
//
// --modules adds a node_modules dir to resolution (for bundling a product
// that lives outside this repo, pass its node_modules).

import { build } from "esbuild"
import { readFileSync, mkdirSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const here = dirname(fileURLToPath(import.meta.url))
const sdk = join(here, "..", "sdk")

const args = process.argv.slice(2)
const entry = args.find((a) => !a.startsWith("--"))
const out = args.includes("--out") ? args[args.indexOf("--out") + 1] : "dist/bundle.js"
const modules = args.includes("--modules") ? [resolve(args[args.indexOf("--modules") + 1])] : []

if (!entry) {
  console.error("usage: node build/bundle.mjs <entry.ts> --out dist/app.js [--modules DIR]")
  process.exit(1)
}

const prelude = readFileSync(join(sdk, "prelude.js"), "utf8")

mkdirSync(dirname(resolve(out)), { recursive: true })

await build({
  entryPoints: [resolve(entry)],
  outfile: resolve(out),
  bundle: true,
  format: "iife",
  platform: "neutral",
  target: "es2022",
  mainFields: ["module", "main"],
  conditions: ["import", "module", "default"],
  nodePaths: modules,
  banner: { js: prelude },
  alias: {
    "node:fs": join(sdk, "node-fs.js"),
    fs: join(sdk, "node-fs.js"),
    "node:path": join(sdk, "node-path.js"),
    path: join(sdk, "node-path.js"),
    "node:crypto": join(sdk, "node-crypto.js"),
    crypto: join(sdk, "node-crypto.js")
  },
  logLevel: "info"
})
