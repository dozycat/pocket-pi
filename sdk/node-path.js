// node:path over sandbox-relative POSIX paths (bundler alias).
export const sep = "/"
export const dirname = (p) => {
  const s = String(p)
  const i = s.lastIndexOf("/")
  return i < 0 ? "." : i === 0 ? "/" : s.slice(0, i)
}
export const basename = (p) => {
  const s = String(p)
  const i = s.lastIndexOf("/")
  return i < 0 ? s : s.slice(i + 1)
}
export const join = (...parts) =>
  parts
    .filter((p) => p !== undefined && p !== null && p !== "")
    .join("/")
    .replace(/\/{2,}/g, "/")
export const resolve = join
export default { sep, dirname, basename, join, resolve }
