// Scratch pad (ui-design §7.2.19): a pen-first drawing surface beside or over
// a learn session. Not a drawing program: a pen, an eraser, three colours,
// one line width, undo and clear. A drawing is kept per session on this
// device (localStorage, never synced and never in the op log) until the
// learner clears it, and can be sent to the tutor as a picture: with the
// reasoning or the answer of an open question, or with the next message.
//
// Strokes are vectors in board units, so undo, the eraser, resizing and the
// dock/overlay switch never lose detail. The board has one fixed aspect
// ratio per session (taken from the panel the first time it opens) and is
// letterboxed in the panel after that. A figure the tutor made (an artifact's
// SVG, e.g. a circuit) can sit under the strokes as the background and is
// part of the picture that is sent.

import { message } from './api.js';
import { cleanSvg } from './artifacts.js';
import { penButtons, penPrefs } from './pen.js';

const $ = (id) => document.getElementById(id);

const BOARD_W = 1200;
const BOARD_H_MIN = 675;
const BOARD_H_MAX = 1600;
const WIDTHS = [3, 6, 12];
const COLORS = ['ink', 'red', 'blue'];
const ERASER_RADIUS = 16;
/** A touch this soon after the pen was last seen is a resting palm. */
const PALM_MS = 800;
const MAX_HISTORY = 100;
const KEY = (id) => `lh.pad.${id}`;
/** The picture that is sent is always dark ink on white paper, whatever the theme. */
const EXPORT = { paper: '#FFFFFF', ink: '#1C1B19', red: '#B3261E', blue: '#1F5FBF' };

let hooks = null;
let els = null;
let wide = null;
let big = null;
let isOpen = false;
let pad = null; // { id, board, strokes, nextId, bg }
let history = [];
let active = null; // the stroke or erase gesture in progress
let saveTimer = null;
let lastPenAt = 0;
let penSideButtons = 0;
let host = null; // the screen the pad is open beside (it gets `pad-docked`)
let ctx = null;
const ui = { tool: 'pen', color: 'ink', width: 6, userOverlay: false };

// ---------- state per session ----------

function fresh(id) {
  return { id, board: null, strokes: [], nextId: 1, bg: null };
}

const num = (v) => (Number.isFinite(v) ? v : 0);

/** A stored pad, or a fresh one when nothing usable is stored. */
function load(id) {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY(id)) ?? 'null');
    if (!raw || !Array.isArray(raw.strokes)) return fresh(id);
    const out = fresh(id);
    if (raw.board && raw.board.w === BOARD_W && Number.isFinite(raw.board.h)) {
      out.board = { w: BOARD_W, h: Math.min(BOARD_H_MAX, Math.max(BOARD_H_MIN, raw.board.h)) };
    }
    out.bg = typeof raw.bg === 'string' ? raw.bg : null;
    for (const s of raw.strokes) {
      if (!COLORS.includes(s?.c) || !Array.isArray(s.p) || s.p.length < 3) continue;
      const p = [];
      for (let i = 0; i + 2 < s.p.length; i += 3) p.push([num(s.p[i]), num(s.p[i + 1]), num(s.p[i + 2])]);
      out.strokes.push({ id: out.nextId++, c: s.c, w: Math.min(24, Math.max(2, num(s.w) || 6)), p });
    }
    return out;
  } catch {
    return fresh(id);
  }
}

function persist() {
  clearTimeout(saveTimer);
  saveTimer = null;
  if (!pad) return;
  const r1 = (v) => Math.round(v * 10) / 10;
  const r2 = (v) => Math.round(v * 100) / 100;
  const data = {
    board: pad.board,
    bg: pad.bg,
    strokes: pad.strokes.map((s) => ({ c: s.c, w: s.w, p: s.p.flatMap(([x, y, pr]) => [r1(x), r1(y), r2(pr)]) })),
  };
  try {
    if (data.strokes.length || data.bg) localStorage.setItem(KEY(pad.id), JSON.stringify(data));
    else localStorage.removeItem(KEY(pad.id));
  } catch {
    status('Couldn’t keep this drawing on the device. It stays until you leave the session.', true);
  }
}

function scheduleSave() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(persist, 400);
}

function usePad(id) {
  if (pad?.id === id) return;
  persist();
  pad = load(id);
  history = [];
  active = null;
}

// ---------- geometry and drawing ----------

const css = (name) => getComputedStyle(els.pad).getPropertyValue(name).trim();

function palette() {
  return { ink: css('--ink'), red: css('--pad-red'), blue: css('--pad-blue') };
}

const widthFactor = (p) => 0.35 + 1.3 * p;
const mid = (a, b) => [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];

/** Segment `j` of a stroke: a quadratic curve through the midpoints of its points. */
function drawSegment(c, stroke, j, colors) {
  const p = stroke.p;
  const from = j === 0 ? p[0] : mid(p[j - 1], p[j]);
  const to = j === p.length - 2 ? p[j + 1] : mid(p[j], p[j + 1]);
  c.strokeStyle = colors[stroke.c];
  c.lineWidth = stroke.w * widthFactor((p[j][2] + p[j + 1][2]) / 2);
  c.beginPath();
  c.moveTo(from[0], from[1]);
  c.quadraticCurveTo(p[j][0], p[j][1], to[0], to[1]);
  c.stroke();
}

function drawDot(c, stroke, colors) {
  const [x, y, pr] = stroke.p[0];
  c.fillStyle = colors[stroke.c];
  c.beginPath();
  c.arc(x, y, (stroke.w * widthFactor(pr)) / 2, 0, Math.PI * 2);
  c.fill();
}

function drawStroke(c, stroke, colors) {
  c.lineCap = 'round';
  c.lineJoin = 'round';
  if (stroke.p.length === 1) drawDot(c, stroke, colors);
  for (let j = 0; j + 1 < stroke.p.length; j++) drawSegment(c, stroke, j, colors);
}

function render() {
  if (!ctx || !pad?.board) return;
  ctx.save();
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, els.canvas.width, els.canvas.height);
  ctx.restore();
  const colors = palette();
  for (const s of pad.strokes) drawStroke(ctx, s, colors);
}

/** Sizes the board to fit the stage and the canvas to the board. */
function fit() {
  if (!isOpen || !pad) return;
  const stage = els.stage;
  const style = getComputedStyle(stage);
  const availW = stage.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
  const availH = stage.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom);
  if (availW < 40 || availH < 40) return;
  if (!pad.board) {
    const h = Math.round((BOARD_W * availH) / availW);
    pad.board = { w: BOARD_W, h: Math.min(BOARD_H_MAX, Math.max(BOARD_H_MIN, h)) };
    scheduleSave();
  }
  const k = Math.min(availW / pad.board.w, availH / pad.board.h);
  const cssW = Math.floor(pad.board.w * k);
  const cssH = Math.floor(pad.board.h * k);
  const dpr = window.devicePixelRatio || 1;
  els.board.style.width = `${cssW}px`;
  els.board.style.height = `${cssH}px`;
  const pxW = Math.round(cssW * dpr);
  const pxH = Math.round(cssH * dpr);
  if (els.canvas.width !== pxW || els.canvas.height !== pxH) {
    els.canvas.width = pxW;
    els.canvas.height = pxH;
  }
  ctx = els.canvas.getContext('2d');
  ctx.setTransform(pxW / pad.board.w, 0, 0, pxH / pad.board.h, 0, 0);
  render();
}

/** A pointer event's position in board units. */
function boardPoint(e) {
  const r = els.canvas.getBoundingClientRect();
  return [((e.clientX - r.left) * pad.board.w) / r.width, ((e.clientY - r.top) * pad.board.h) / r.height];
}

// ---------- erasing and history ----------

function nearSegment(px, py, a, b) {
  const dx = b[0] - a[0];
  const dy = b[1] - a[1];
  const len = dx * dx + dy * dy;
  const t = len ? Math.max(0, Math.min(1, ((px - a[0]) * dx + (py - a[1]) * dy) / len)) : 0;
  return Math.hypot(px - (a[0] + t * dx), py - (a[1] + t * dy));
}

function hits(stroke, x, y) {
  const reach = ERASER_RADIUS + (stroke.w * 1.65) / 2;
  const p = stroke.p;
  if (p.length === 1) return Math.hypot(x - p[0][0], y - p[0][1]) <= reach;
  for (let i = 0; i + 1 < p.length; i++) if (nearSegment(x, y, p[i], p[i + 1]) <= reach) return true;
  return false;
}

/** Erases what the eraser touches moving to (x, y), including the ground since its last position. */
function eraseTo(x, y) {
  const [fromX, fromY] = active.last ?? [x, y];
  active.last = [x, y];
  const steps = Math.max(1, Math.ceil(Math.hypot(x - fromX, y - fromY) / (ERASER_RADIUS / 2)));
  const gone = new Set();
  for (let i = 1; i <= steps; i++) {
    const px = fromX + ((x - fromX) * i) / steps;
    const py = fromY + ((y - fromY) * i) / steps;
    for (const s of pad.strokes) if (!gone.has(s) && hits(s, px, py)) gone.add(s);
  }
  if (!gone.size) return;
  pad.strokes = pad.strokes.filter((s) => !gone.has(s));
  active.removed.push(...gone);
  render();
}

function record(entry) {
  history.push(entry);
  if (history.length > MAX_HISTORY) history.shift();
  updateButtons();
}

function undo() {
  const entry = history.pop();
  if (!entry) return;
  if (entry.add) {
    pad.strokes = pad.strokes.filter((s) => !entry.add.includes(s.id));
  } else {
    pad.strokes = [...pad.strokes, ...entry.removed].sort((a, b) => a.id - b.id);
  }
  render();
  persist();
  updateButtons();
  status('');
}

function clearAll() {
  if (!pad.strokes.length) return;
  record({ removed: pad.strokes });
  pad.strokes = [];
  render();
  persist();
  status('Cleared. Undo brings it back.');
}

// ---------- pointer input ----------

function accepts(e) {
  const prefs = penPrefs();
  if (e.pointerType === 'pen') return true;
  if (e.pointerType === 'mouse') return prefs.inputs !== 'pen' && e.button === 0;
  if (e.pointerType === 'touch') {
    return prefs.inputs === 'all' && !(prefs.palm && performance.now() - lastPenAt < PALM_MS);
  }
  return false;
}

function pressureOf(e, prefs) {
  return e.pointerType === 'pen' && prefs.pressure && e.pressure > 0 ? e.pressure : 0.5;
}

function onDown(e) {
  if (!pad?.board || active || !accepts(e)) return;
  const prefs = penPrefs();
  const b = penButtons(e.buttons);
  let tool = ui.tool;
  if (e.pointerType === 'pen') {
    // A press on a button alone (or the eraser end set to another action)
    // is not a stroke; the button's own action already ran.
    if (!b.tip && !b.eraser) return;
    if (!b.tip && b.eraser && prefs.eraser !== 'erase') return;
    const held =
      (b.eraser && prefs.eraser === 'erase') ||
      (b.barrel && prefs.barrel === 'erase') ||
      (b.barrel2 && prefs.barrel2 === 'erase');
    if (held) tool = 'erase';
  }
  e.preventDefault();
  try {
    els.canvas.setPointerCapture(e.pointerId);
  } catch {
    // The pointer is already gone; the stroke still draws from the events it sends.
  }
  const [x, y] = boardPoint(e);
  active = { id: e.pointerId, tool, stroke: null, removed: [], prefs };
  els.pad.classList.toggle('erasing', tool === 'erase');
  if (tool === 'erase') {
    eraseTo(x, y);
    return;
  }
  const stroke = { id: pad.nextId++, c: ui.color, w: ui.width, p: [[x, y, pressureOf(e, prefs)]] };
  active.stroke = stroke;
  pad.strokes.push(stroke);
  ctx.lineCap = 'round';
  drawDot(ctx, stroke, palette());
}

function onMove(e) {
  if (!active || e.pointerId !== active.id) return;
  const events = e.getCoalescedEvents?.() ?? [e];
  for (const ev of events.length ? events : [e]) {
    const [x, y] = boardPoint(ev);
    if (active.tool === 'erase') {
      eraseTo(x, y);
      continue;
    }
    const s = active.stroke;
    const last = s.p[s.p.length - 1];
    if (Math.hypot(x - last[0], y - last[1]) < 0.8) continue;
    s.p.push([x, y, pressureOf(ev, active.prefs)]);
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    drawSegment(ctx, s, s.p.length - 2, palette());
  }
}

function endGesture(cancelled) {
  if (!active) return;
  const g = active;
  active = null;
  els.pad.classList.remove('erasing');
  if (g.tool === 'erase') {
    if (g.removed.length) {
      if (cancelled) pad.strokes = [...pad.strokes, ...g.removed].sort((a, b) => a.id - b.id);
      else record({ removed: g.removed });
    }
  } else if (cancelled) {
    pad.strokes = pad.strokes.filter((s) => s !== g.stroke);
  } else {
    record({ add: [g.stroke.id] });
  }
  if (cancelled || g.stroke) render();
  scheduleSave();
  updateButtons();
}

// ---------- pen buttons ----------

const SIDE_BUTTONS = [
  [2, 'barrel'],
  [4, 'barrel2'],
  [32, 'eraser'],
];

function cycleColor() {
  setColor(COLORS[(COLORS.indexOf(ui.color) + 1) % COLORS.length]);
}

function cycleWidth() {
  setWidth(WIDTHS.find((w) => w > ui.width) ?? WIDTHS[0]);
}

function runAction(action) {
  if (action === 'pad') {
    // Not while a dialog (Settings) has the page.
    if (!document.querySelector('dialog[open]')) toggle();
    return undefined;
  }
  if (!isOpen) return undefined;
  if (action === 'undo') undo();
  else if (action === 'color') cycleColor();
  else if (action === 'width') cycleWidth();
  return undefined;
}

/**
 * Watches every pen event on the page: remembers when the pen was last near
 * (for palm rejection) and runs a button's action when it goes down. `erase`
 * is a hold, read per stroke in `onDown`, not an action on press.
 */
function trackPen(e) {
  if (e.pointerType !== 'pen') return;
  lastPenAt = performance.now();
  const now = e.buttons & 0b100110;
  const pressed = now & ~penSideButtons;
  penSideButtons = now;
  if (!pressed) return;
  const prefs = penPrefs();
  for (const [bit, name] of SIDE_BUTTONS) if (pressed & bit) runAction(prefs[name]);
}

/** A barrel button reads as a right click on most systems; the pad owns it. */
function onContextMenu(e) {
  if (isOpen && els.pad.contains(e.target)) e.preventDefault();
  else if (e.pointerType === 'pen' && SIDE_BUTTONS.some(([, name]) => penPrefs()[name] === 'pad')) e.preventDefault();
}

// ---------- toolbar ----------

function setTool(tool) {
  ui.tool = tool;
  els.penTool.setAttribute('aria-pressed', String(tool === 'pen'));
  els.eraserTool.setAttribute('aria-pressed', String(tool === 'erase'));
}

function setColor(color) {
  ui.color = color;
  for (const b of els.swatches) b.setAttribute('aria-pressed', String(b.dataset.color === color));
  if (ui.tool === 'erase') setTool('pen');
}

function setWidth(width) {
  ui.width = width;
  els.width.value = String(width);
}

function updateButtons() {
  if (!els) return;
  els.undo.disabled = history.length === 0;
  els.clear.disabled = !pad || pad.strokes.length === 0;
}

function status(text, error = false) {
  if (!els) return;
  els.status.textContent = text;
  els.status.classList.toggle('error', error);
}

// ---------- figure background ----------

function figures() {
  try {
    return hooks.figures() ?? [];
  } catch {
    return [];
  }
}

function currentFigure() {
  return pad?.bg ? figures().find((f) => f.id === pad.bg) ?? null : null;
}

function renderBackground() {
  const layer = els.bgLayer;
  layer.replaceChildren();
  const fig = currentFigure();
  if (!fig) return;
  const box = document.createElement('div');
  box.innerHTML = cleanSvg(fig.svg);
  const svg = box.querySelector('svg');
  if (!svg) return;
  svg.removeAttribute('width');
  svg.removeAttribute('height');
  svg.removeAttribute('role');
  svg.removeAttribute('aria-label');
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
  layer.append(svg);
}

/** Rebuilds the "Draw on" list from the session's figures; hides it when there are none. */
function refreshFigures() {
  if (!els) return;
  const list = figures();
  if (pad?.bg && !list.some((f) => f.id === pad.bg)) {
    pad.bg = null;
    renderBackground();
  }
  els.bgPick.hidden = list.length === 0;
  const keep = pad?.bg ?? '';
  const want = ['', ...list.map((f) => f.id)];
  const have = [...els.bg.options].map((o) => o.value);
  if (want.join('\n') !== have.join('\n') || list.some((f, i) => els.bg.options[i + 1]?.textContent !== f.title)) {
    els.bg.replaceChildren(new Option('Plain paper', ''), ...list.map((f) => new Option(f.title, f.id)));
  }
  els.bg.value = keep;
}

/** The background as an image drawn into `c` (the export), fitted like the inline SVG. */
async function paintBackground(c, w, h) {
  const fig = currentFigure();
  if (!fig) return;
  const box = document.createElement('div');
  box.innerHTML = cleanSvg(fig.svg);
  const svg = box.querySelector('svg');
  if (!svg) return;
  const vb = svg.viewBox?.baseVal;
  const sw = (vb && vb.width) || parseFloat(svg.getAttribute('width')) || 300;
  const sh = (vb && vb.height) || parseFloat(svg.getAttribute('height')) || 150;
  svg.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
  svg.setAttribute('width', String(sw));
  svg.setAttribute('height', String(sh));
  const xml = new XMLSerializer().serializeToString(svg).replace(/currentColor/gi, EXPORT.ink);
  const img = new Image();
  img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(xml)}`;
  await img.decode();
  const k = Math.min(w / sw, h / sh);
  c.drawImage(img, (w - sw * k) / 2, (h - sh * k) / 2, sw * k, sh * k);
}

/** The drawing as a PNG data URL: dark ink on white, the figure underneath. */
async function exportPng() {
  const { w, h } = pad.board;
  const canvas = document.createElement('canvas');
  canvas.width = w;
  canvas.height = h;
  const c = canvas.getContext('2d');
  c.fillStyle = EXPORT.paper;
  c.fillRect(0, 0, w, h);
  try {
    await paintBackground(c, w, h);
  } catch {
    throw new Error('Couldn’t draw the figure into the picture. Pick “Plain paper” to send the drawing alone.');
  }
  for (const s of pad.strokes) drawStroke(c, s, EXPORT);
  return canvas.toDataURL('image/png');
}

// ---------- sending ----------

function closeSendMenu() {
  els.sendMenu.hidden = true;
  els.send.setAttribute('aria-expanded', 'false');
}

async function sendTo(target) {
  closeSendMenu();
  if (!pad.strokes.length) {
    status('Draw something first.');
    return;
  }
  if (hooks.canSeeImages?.() === false) {
    status('This model can’t see images. Choose a Tutor model that can, in Settings.', true);
    return;
  }
  els.send.disabled = true;
  status('Sending…');
  try {
    const png = await exportPng();
    const done = await hooks.send(target.id, png, { figure: currentFigure()?.title ?? null });
    status(done || 'Sent.');
  } catch (err) {
    status(message(err), true);
  } finally {
    els.send.disabled = false;
  }
}

function onSend() {
  const targets = hooks.targets();
  if (!targets.length) {
    status('Nothing is open to send this with right now. Come back when there is a question or an answer box.');
    return;
  }
  if (targets.length === 1) {
    sendTo(targets[0]);
    return;
  }
  if (!els.sendMenu.hidden) {
    closeSendMenu();
    return;
  }
  els.sendMenu.replaceChildren(
    ...targets.map((t) => {
      const item = document.createElement('button');
      item.type = 'button';
      item.className = 'menu-item';
      item.setAttribute('role', 'menuitem');
      item.textContent = t.label;
      item.addEventListener('click', () => {
        sendTo(t);
        els.send.focus({ preventScroll: true });
      });
      return item;
    }),
  );
  els.sendMenu.hidden = false;
  els.send.setAttribute('aria-expanded', 'true');
  els.sendMenu.querySelector('button')?.focus({ preventScroll: true });
}

function onSendMenuKey(e) {
  const items = [...els.sendMenu.querySelectorAll('button')];
  const at = items.indexOf(document.activeElement);
  if (e.key === 'Escape') {
    e.preventDefault();
    e.stopPropagation();
    closeSendMenu();
    els.send.focus({ preventScroll: true });
  } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    items[(at + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length]?.focus({ preventScroll: true });
  }
}

// ---------- layout: dock beside the lesson, or overlay ----------

function mode() {
  return wide.matches && !ui.userOverlay ? 'dock' : 'overlay';
}

function applyMode() {
  const m = mode();
  els.pad.dataset.mode = m;
  host?.classList.toggle('pad-docked', isOpen && m === 'dock');
  els.mode.hidden = !wide.matches;
  const overlay = m === 'overlay';
  const label = overlay ? 'Dock beside the lesson' : 'Show over the lesson';
  els.mode.setAttribute('aria-label', label);
  els.mode.title = label;
  // SVG elements have no `hidden` property: toggle the attribute.
  els.mode.querySelector('.pad-mode-expand').toggleAttribute('hidden', overlay);
  els.mode.querySelector('.pad-mode-dock').toggleAttribute('hidden', !overlay);
}

// ---------- open and close ----------

const toggleIn = (section) => els.toggles.find((t) => section?.contains(t)) ?? null;

function syncToggle() {
  for (const t of els.toggles) {
    t.setAttribute('aria-expanded', String(isOpen && t === toggleIn(host)));
    t.classList.toggle('active', isOpen && t === toggleIn(host));
  }
}

/** Shows or hides the pad's toggle for this screen, and closes the pad when it no longer applies. */
export function refresh() {
  if (!els) return;
  const usable = Boolean(hooks.canUse()) && big.matches;
  const section = hooks.host();
  for (const t of els.toggles) t.hidden = !(usable && section?.contains(t));
  if (!usable && isOpen) close();
  if (isOpen) refreshFigures();
  syncToggle();
}

export function isPadOpen() {
  return isOpen;
}

export function open() {
  if (!els || isOpen || !hooks.canUse() || !big.matches) return;
  const id = hooks.sessionId();
  if (!id) return;
  usePad(id);
  isOpen = true;
  host = hooks.host();
  ui.userOverlay = false;
  els.pad.hidden = false;
  applyMode();
  refreshFigures();
  renderBackground();
  setTool(ui.tool);
  setColor(ui.color);
  setWidth(ui.width);
  updateButtons();
  status('');
  syncToggle();
  fit();
  els.title.focus({ preventScroll: true });
}

export function close({ focus = false } = {}) {
  if (!els || !isOpen) return;
  endGesture(true);
  persist();
  closeSendMenu();
  isOpen = false;
  els.pad.hidden = true;
  host?.classList.remove('pad-docked');
  syncToggle();
  if (focus) toggleIn(host)?.focus({ preventScroll: true });
  host = null;
}

export function toggle() {
  if (isOpen) close({ focus: true });
  else open();
}

/** The session was left: save, close, forget which session the pad was on. */
export function detach() {
  if (!els) return;
  close();
  persist();
  pad = null;
  history = [];
  for (const t of els.toggles) t.hidden = true;
}

function onKey(e) {
  if (e.key === 'Escape' && !els.sendMenu.hidden) return;
  if (e.key === 'Escape' && els.pad.dataset.mode === 'overlay') {
    e.preventDefault();
    close({ focus: true });
    return;
  }
  const field = e.target.closest?.('select, input[type="range"]');
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === 'z' && !field) {
    e.preventDefault();
    undo();
  }
}

// ---------- setup ----------

/**
 * `h`: `sessionId()`, `canUse()` (a live learn or practice session is
 * showing), `host()` (that screen's section, which gets `pad-docked` and
 * holds the toggle that applies),
 * `figures()` ([{ id, title, svg }]), `targets()` ([{ id, label }]: where a
 * drawing can go right now), `send(targetId, pngDataUrl, { figure })`
 * (resolves to a short status line) and `canSeeImages()`.
 */
export function initScratchpad(h) {
  hooks = h;
  els = {
    pad: $('pad'),
    toggles: [...document.querySelectorAll('.pad-toggle')],
    title: $('pad-title'),
    mode: $('pad-mode'),
    close: $('pad-close'),
    penTool: $('pad-pen'),
    eraserTool: $('pad-eraser'),
    swatches: [...document.querySelectorAll('.pad-swatch')],
    width: $('pad-width'),
    undo: $('pad-undo'),
    clear: $('pad-clear'),
    bgPick: $('pad-bg-pick'),
    bg: $('pad-bg'),
    stage: $('pad-stage'),
    board: $('pad-board'),
    bgLayer: $('pad-bg-layer'),
    canvas: $('pad-canvas'),
    send: $('pad-send'),
    sendMenu: $('pad-send-menu'),
    status: $('pad-status'),
  };
  if (!els.pad || !els.toggles.length) {
    els = null;
    return;
  }
  wide = window.matchMedia('(min-width: 900px)');
  big = window.matchMedia('(min-width: 600px) and (min-height: 480px)');

  for (const t of els.toggles) t.addEventListener('click', toggle);
  els.close.addEventListener('click', () => close({ focus: true }));
  els.mode.addEventListener('click', () => {
    ui.userOverlay = mode() === 'dock';
    applyMode();
  });
  els.penTool.addEventListener('click', () => setTool('pen'));
  els.eraserTool.addEventListener('click', () => setTool('erase'));
  for (const b of els.swatches) b.addEventListener('click', () => setColor(b.dataset.color));
  els.width.addEventListener('input', () => setWidth(Number(els.width.value)));
  els.undo.addEventListener('click', undo);
  els.clear.addEventListener('click', clearAll);
  els.bg.addEventListener('change', () => {
    pad.bg = els.bg.value || null;
    renderBackground();
    scheduleSave();
  });
  for (const type of ['focus', 'pointerdown']) els.bg.addEventListener(type, refreshFigures);
  els.send.addEventListener('click', onSend);
  els.sendMenu.addEventListener('keydown', onSendMenuKey);
  document.addEventListener('pointerdown', (e) => {
    if (!els.sendMenu.hidden && !els.sendMenu.contains(e.target) && !els.send.contains(e.target)) closeSendMenu();
  });
  els.pad.addEventListener('keydown', onKey);

  els.canvas.addEventListener('pointerdown', onDown);
  els.canvas.addEventListener('pointermove', onMove);
  els.canvas.addEventListener('pointerup', () => endGesture(false));
  els.canvas.addEventListener('pointercancel', () => endGesture(true));
  els.canvas.addEventListener('lostpointercapture', () => endGesture(false));

  for (const type of ['pointerdown', 'pointermove', 'pointerup']) {
    document.addEventListener(type, trackPen, true);
  }
  document.addEventListener('contextmenu', onContextMenu, true);

  new ResizeObserver(fit).observe(els.stage);
  wide.addEventListener('change', () => {
    applyMode();
    fit();
  });
  big.addEventListener('change', refresh);
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', render);
  window.addEventListener('pagehide', persist);
}
