/// Tauri rejects commands with the structured `SerializedError` shape
/// (`{kind, message, status?}`), not a string. `String(e)` on that yields
/// "[object Object]", which is what the UI used to show for every failure —
/// hiding the one field that matters. Always render errors through this.

type WireError = { kind?: string; message?: string; status?: number };

export function errText(e: unknown): string {
  if (e == null) return "未知错误";
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  if (typeof e === "object") {
    const w = e as WireError;
    if (typeof w.message === "string" && w.message) {
      return w.status ? `${w.message}（HTTP ${w.status}）` : w.message;
    }
    try {
      return JSON.stringify(e);
    } catch {
      return String(e);
    }
  }
  return String(e);
}
