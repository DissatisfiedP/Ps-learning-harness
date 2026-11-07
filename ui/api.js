// Thin wrapper over Tauri IPC. Every backend call goes through `call`.

const tauri = window.__TAURI__;

export const available = Boolean(tauri?.core?.invoke);

export function call(command, args = {}) {
  if (!available) {
    return Promise.reject(new Error("P's Learning Harness must run inside its desktop or mobile app."));
  }
  return tauri.core.invoke(command, args);
}

/** Channel for streamed events; `onEvent` receives each parsed payload. */
export function channel(onEvent) {
  const ch = new tauri.core.Channel();
  ch.onmessage = onEvent;
  return ch;
}

/** IPC rejections arrive as plain strings; normalise for display. */
export function message(err) {
  if (typeof err === 'string') return err;
  return err?.message ?? String(err);
}
