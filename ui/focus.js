// Focus mode (ui-design §7.8): while a session is open the window is full
// screen and desktop notifications are silenced; after 50 active minutes a
// break nudge waits for the next turn boundary. Preferences are UI-only
// (localStorage, like `lh.theme`) and default on.

import { available, call, message } from './api.js';

const KEYS = { fullscreen: 'lh.focus.fullscreen', dnd: 'lh.focus.dnd', breaks: 'lh.focus.breaks' };
/** Debug override for the break interval, in minutes (e.g. 0.01). */
const BREAK_DEBUG_KEY = 'lh.debug.breakMinutes';
const BREAK_MINUTES = 50;
/** A pause this long (app hidden, blurred, or no session open) counts as the break. */
const PAUSE_RESETS_MS = 5 * 60_000;
export const NUDGE_TEXT = '50 minutes in. Stand up for a minute.';

export function focusPref(name) {
  try {
    return localStorage.getItem(KEYS[name]) !== 'off';
  } catch {
    return true;
  }
}

export function setFocusPref(name, on) {
  try {
    localStorage.setItem(KEYS[name], on ? 'on' : 'off');
  } catch {
    // Not persisted; the choice still applies to this window.
  }
}

/** The notification backend name (e.g. "mako"), or null when unsupported. */
export async function dndSupport() {
  if (!available) return null;
  try {
    return (await call('focus_dnd_support')) ?? null;
  } catch {
    return null;
  }
}

// ---------- full screen ----------

let onFullscreen = () => {};
let fsKnown = false;

function tauriWindow() {
  try {
    return window.__TAURI__?.window?.getCurrentWindow?.() ?? null;
  } catch {
    return null;
  }
}

async function isFullscreen() {
  const w = tauriWindow();
  if (!w) return Boolean(document.fullscreenElement);
  try {
    return await w.isFullscreen();
  } catch {
    return fsKnown;
  }
}

async function setFullscreen(on) {
  const w = tauriWindow();
  try {
    if (w) await w.setFullscreen(on);
    else if (on) await document.documentElement.requestFullscreen?.();
    else if (document.fullscreenElement) await document.exitFullscreen();
  } catch {
    // Refused (no permission, no user gesture): stay as is.
  }
  fsKnown = await isFullscreen();
  onFullscreen(fsKnown);
}

export async function toggleFullscreen() {
  await setFullscreen(!(await isFullscreen()));
}

/** Reports the current state to the `onFullscreen` hook (e.g. on menu open). */
export async function refreshFullscreen() {
  fsKnown = await isFullscreen();
  onFullscreen(fsKnown);
}

// ---------- session enter / leave ----------

let inSession = false;
let generation = 0;
/** Full-screen state before the session; restored on leave. */
let before = null;
/** Serialises DND calls so an "off" never overtakes its "on". */
let dndChain = Promise.resolve();
let dndOn = false;

function dnd(on) {
  if (dndOn === on) return;
  dndOn = on;
  dndChain = dndChain
    .then(() => call('focus_dnd', { on }))
    .catch((e) => console.warn('focus_dnd:', message(e)));
}

export async function enterSession() {
  if (inSession) return;
  inSession = true;
  const gen = ++generation;
  clock();
  if (available && focusPref('dnd')) dnd(true);
  const was = await isFullscreen();
  if (gen !== generation) return;
  before = was;
  if (focusPref('fullscreen') && !was) await setFullscreen(true);
  else onFullscreen(was);
}

export async function leaveSession() {
  if (!inSession) return;
  inSession = false;
  generation++;
  clock();
  if (dndOn) dnd(false);
  const to = before;
  before = null;
  if (to !== null && (await isFullscreen()) !== to) await setFullscreen(to);
}

// ---------- break nudge ----------

let activeMs = 0;
let since = null;
let pausedAt = null;

/** Starts or pauses the clock: it runs only while a session is shown and the window is visible and focused. */
function clock() {
  const active = inSession && document.visibilityState === 'visible' && document.hasFocus();
  const now = performance.now();
  if (active && since === null) {
    if (pausedAt !== null && now - pausedAt >= PAUSE_RESETS_MS) activeMs = 0;
    pausedAt = null;
    since = now;
  } else if (!active && since !== null) {
    activeMs += now - since;
    since = null;
    pausedAt = now;
  }
}

function breakMs() {
  try {
    const debug = parseFloat(localStorage.getItem(BREAK_DEBUG_KEY));
    if (debug > 0) return debug * 60_000;
  } catch {
    // Storage unavailable: the default interval.
  }
  return BREAK_MINUTES * 60_000;
}

/** At a turn boundary: true (and the clock restarts) when a nudge is due. */
export function takeNudge() {
  if (!inSession || !focusPref('breaks')) return false;
  clock();
  const now = performance.now();
  const total = activeMs + (since === null ? 0 : now - since);
  if (total < breakMs()) return false;
  activeMs = 0;
  if (since !== null) since = now;
  return true;
}

export function nudgeEl(text) {
  const el = document.createElement('p');
  el.className = 'nudge';
  el.textContent = text;
  return el;
}

/** `hooks.onFullscreen(on)`: the window's full-screen state changed or was read. */
export function initFocus(hooks = {}) {
  onFullscreen = hooks.onFullscreen ?? onFullscreen;
  document.addEventListener('visibilitychange', clock);
  window.addEventListener('focus', clock);
  window.addEventListener('blur', clock);
  document.addEventListener('fullscreenchange', () => onFullscreen(Boolean(document.fullscreenElement)));
  document.addEventListener('keydown', (e) => {
    if (!inSession || e.key !== 'F11' || e.defaultPrevented) return;
    e.preventDefault();
    toggleFullscreen();
  });
}
