// node:fs over the pi surface (bundler alias). Sync by design — pi ops are
// synchronous (Law 2), which is exactly what the event-log pattern needs
// (a committed event is durable before the pipeline continues). All paths
// resolve inside the host's --root sandbox.
/* global pi */

const enoent = (path) =>
  Object.assign(new Error(`ENOENT: no such file or directory, open '${path}'`), {
    code: "ENOENT",
    path
  })

export const readFileSync = (path, _encoding) => {
  const text = pi.fsRead(String(path))
  // absent files come back null-ish (the host maps None to undefined)
  if (text == null) throw enoent(path)
  return text
}
export const writeFileSync = (path, data) => {
  pi.fsWrite(String(path), String(data))
}
export const appendFileSync = (path, data) => {
  pi.fsAppend(String(path), String(data))
}
export const existsSync = (path) => pi.fsExists(String(path))
export const rmSync = (path, _options) => {
  pi.fsRemove(String(path))
}
export const unlinkSync = rmSync
// Parents are created by fsWrite/fsAppend; mkdir is bookkeeping-free here.
export const mkdirSync = (_path, _options) => undefined

export default {
  readFileSync,
  writeFileSync,
  appendFileSync,
  existsSync,
  rmSync,
  unlinkSync,
  mkdirSync
}
