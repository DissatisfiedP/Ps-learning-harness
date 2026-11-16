// Scratch pad (ui-design §7.2.19): a pen-first drawing surface beside or over
// a learn session. Not a drawing program: a pen, an eraser, three colours,
// one line width, undo and clear. A drawing is kept per session on this
// device (localStorage, never synced and never in the op log) until the
// learner clears it, and can be sent to the tutor as a picture: with the
// reasoning or the answer of an open question, or with the next message.
//
// Strokes are vectors in world units on a board with no edges: the learner
// pans and zooms (pinch, Ctrl/⌘+scroll, the zoom buttons) and the canvas is
// redrawn from the vectors at the screen's resolution, so nothing is ever
// lost to pixels. The canvas fills the panel; the view keeps its top-left
// corner and zoom when the panel changes size, so the dock/overlay switch
// shows more of the board instead of scaling the drawing. A new stroke's
// width is the slider's width on screen at the zoom it was drawn at. A
// figure the tutor made (an artifact's SVG, e.g. a circuit) can sit under
// the strokes as the background, placed where the learner was looking when
// they picked it. The picture that is sent is the drawing's bounding box
// (strokes and figure) with a margin, scaled to a fixed long side.

import { message } from './api.js';
import { cleanSvg } from './artifacts.js';
import { penButtons, penPrefs } from './pen.js';

const $ = (id) => document.getElementById(id);

/** Pads saved before zooming had a fixed board this many units wide. */
const LEGACY_BOARD_W = 1200;
const WIDTHS = [3, 6, 12];
const COLORS = ['ink', 'red', 'blue'];
/** On screen, in CSS px, at any zoom. */
const ERASER_RADIUS = 16;
const Z_MIN = 0.01;
const Z_MAX = 100;
const ZOOM_STEP = Math.SQRT2;
/** CSS px kept free around the drawing when zooming to fit it. */
const FIT_MARGIN = 24;
/** The long side of the picture that is sent, and the white margin around the drawing in it, in px. */
const EXPORT_LONG = 1600;
const EXPORT_PAD = 32;
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
let pad = null; // { id, strokes, nextId, bg, bgRect, bgFrame, view: { x, y, z } }
let history = [];
let active = null; // the stroke or erase gesture in progress
let saveTimer = null;
let lastPenAt = 0;
let penSideButtons = 0;
let host = null; // the screen the pad is open beside (it gets `pad-docked`)
let ctx = null;
let dpr = 1;
/** Pointers moving the view (a finger, a pinch, a middle-button or pen-only mouse drag): id → stage point. */
const nav = { pts: new Map(), start: null };
/** The background figure's own size, from its SVG. */
let bgSize = null;
let gestureZ = 1;
const ui = { tool: 'pen', color: 'ink', width: 6, userOverlay: false };

// ---------- state per session ----------

function fresh(id) {
  return { id, strokes: [], nextId: 1, bg: null, bgRect: null, bgFrame: null, view: null };
}

const num = (v) => (Number.isFinite(v) ? v : 0);
const finite = (...vs) => vs.every(Number.isFinite);
const clampZ = (z) => Math.min(Z_MAX, Math.max(Z_MIN, z));

/** A stored pad, or a fresh one when nothing usable is stored. */
function load(id) {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY(id)) ?? 'null');
    if (!raw || !Array.isArray(raw.strokes)) return fresh(id);
    const out = fresh(id);
    const v = raw.view;
    if (v && finite(v.x, v.y, v.z) && v.z > 0) out.view = { x: v.x, y: v.y, z: clampZ(v.z) };
    out.bg = typeof raw.bg === 'string' ? raw.bg : null;
    const r = raw.bgRect;
    if (out.bg && r && finite(r.x, r.y, r.w, r.h) && r.w > 0 && r.h > 0) {
      out.bgRect = { x: r.x, y: r.y, w: r.w, h: r.h };
    } else if (out.bg && raw.board && Number.isFinite(raw.board.h) && raw.board.h > 0) {
      // Saved before zooming: the figure filled the old fixed board.
      out.bgFrame = { x: 0, y: 0, w: LEGACY_BOARD_W, h: raw.board.h };
    }
    for (const s of raw.strokes) {
      if (!COLORS.includes(s?.c) || !Array.isArray(s.p) || s.p.length < 3) continue;
      const p = [];
      for (let i = 0; i + 2 < s.p.length; i += 3) p.push([num(s.p[i]), num(s.p[i + 1]), num(s.p[i + 2])]);
      out.strokes.push({ id: out.nextId++, c: s.c, w: num(s.w) > 0 ? s.w : 6, p });
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
  const r2 = (v) => Math.round(v * 100) / 100;
  // Coordinates keep a 64th of the stroke's width: as fine as it was drawn, at any zoom.
  const places = (w) => Math.min(15, Math.max(0, Math.ceil(-Math.log10(w / 64))));
  const data = {
    view: pad.view,
    bg: pad.bg,
    bgRect: pad.bg ? pad.bgRect : null,
    strokes: pad.strokes.map((s) => {
      const d = places(s.w);
      const r = (v) => Number(v.toFixed(d));
      return { c: s.c, w: Number(s.w.toPrecision(4)), p: s.p.flatMap(([x, y, pr]) => [r(x), r(y), r2(pr)]) };
    }),
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

/** A stroke's extent in world units, ink included; kept once the stroke is finished. */
function boxOf(s) {
  if (s.box) return s.box;
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const [x, y, pr] of s.p) {
    const r = (s.w * widthFactor(pr)) / 2;
    x0 = Math.min(x0, x - r);
    y0 = Math.min(y0, y - r);
    x1 = Math.max(x1, x + r);
    y1 = Math.max(y1, y + r);
  }
  const box = { x0, y0, x1, y1 };
  if (s !== active?.stroke) s.box = box;
  return box;
}

/** What there is to see, in world units: every stroke and the figure under them; null when empty. */
function contentBox() {
  const boxes = pad.strokes.map(boxOf);
  const r = currentFigure() && pad.bgRect;
  if (r) boxes.push({ x0: r.x, y0: r.y, x1: r.x + r.w, y1: r.y + r.h });
  if (!boxes.length) return null;
  const x0 = Math.min(...boxes.map((b) => b.x0));
  const y0 = Math.min(...boxes.map((b) => b.y0));
  return { x: x0, y: y0, w: Math.max(...boxes.map((b) => b.x1)) - x0, h: Math.max(...boxes.map((b) => b.y1)) - y0 };
}

function render() {
  if (!ctx || !pad?.view) return;
  const { x, y, z } = pad.view;
  const k = z * dpr;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, els.canvas.width, els.canvas.height);
  ctx.setTransform(k, 0, 0, k, -x * k, -y * k);
  const right = x + els.canvas.width / k;
  const bottom = y + els.canvas.height / k;
  const colors = palette();
  for (const s of pad.strokes) {
    const b = boxOf(s);
    if (b.x1 >= x && b.x0 <= right && b.y1 >= y && b.y0 <= bottom) drawStroke(ctx, s, colors);
  }
  placeBackground();
}

const stageSize = () => [els.stage.clientWidth, els.stage.clientHeight];

/** Sizes the canvas to the stage. The view keeps its corner and zoom, so a bigger panel shows more of the board. */
function fit() {
  if (!isOpen || !pad) return;
  const [w, h] = stageSize();
  if (w < 40 || h < 40) return;
  dpr = window.devicePixelRatio || 1;
  const pxW = Math.round(w * dpr);
  const pxH = Math.round(h * dpr);
  if (els.canvas.width !== pxW || els.canvas.height !== pxH) {
    els.canvas.width = pxW;
    els.canvas.height = pxH;
  }
  ctx = els.canvas.getContext('2d');
  if (!pad.view) {
    // First open on this device, or a pad saved before zooming: show all of it.
    pad.view = { x: 0, y: 0, z: 1 };
    placeBackground();
    if (contentBox()) zoomToFit();
  }
  render();
  updateZoom();
}

/** A pointer event's position on the stage, in CSS px. */
function stagePoint(e, r = els.canvas.getBoundingClientRect()) {
  return [e.clientX - r.left, e.clientY - r.top];
}

function toWorld([sx, sy]) {
  const { x, y, z } = pad.view;
  return [x + sx / z, y + sy / z];
}

// ---------- the view: pan and zoom ----------

function setView(x, y, z) {
  pad.view = { x, y, z };
  render();
  updateZoom();
  scheduleSave();
}

/** Zooms to `z`, keeping the board point under `at` (a stage point; the centre by default) in place. */
function zoomTo(z, at = stageSize().map((n) => n / 2)) {
  if (!pad?.view) return;
  const nz = clampZ(z);
  const [wx, wy] = toWorld(at);
  setView(wx - at[0] / nz, wy - at[1] / nz, nz);
}

function zoomBy(factor) {
  if (pad?.view) zoomTo(pad.view.z * factor);
}

/** Shows the whole drawing (and its figure) as large as the stage allows; an empty pad goes back to 100%. */
function zoomToFit() {
  if (!pad?.view) return;
  const box = contentBox();
  if (!box) {
    setView(0, 0, 1);
    return;
  }
  const [w, h] = stageSize();
  const z = clampZ(Math.min((w - 2 * FIT_MARGIN) / Math.max(box.w, 1e-9), (h - 2 * FIT_MARGIN) / Math.max(box.h, 1e-9)));
  setView(box.x + box.w / 2 - w / (2 * z), box.y + box.h / 2 - h / (2 * z), z);
}

function updateZoom() {
  if (!els || !pad?.view) return;
  const z = pad.view.z;
  const pct = `${Math.round(z * 100)}%`;
  els.zoomFit.textContent = pct;
  els.zoomFit.setAttribute('aria-label', `Zoom ${pct}. Fit the drawing`);
  els.zoomIn.disabled = z >= Z_MAX;
  els.zoomOut.disabled = z <= Z_MIN;
}

/** Ctrl/⌘+wheel (and a trackpad pinch, which arrives as one) zooms at the pointer; the wheel alone pans. */
function onWheel(e) {
  if (!pad?.view) return;
  e.preventDefault();
  const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? els.stage.clientHeight : 1;
  const dx = e.deltaX * unit;
  const dy = e.deltaY * unit;
  if (e.ctrlKey || e.metaKey) {
    zoomTo(pad.view.z * 2 ** (-Math.max(-50, Math.min(50, dy)) / 100), stagePoint(e));
    return;
  }
  const [px, py] = e.shiftKey && !dx ? [dy, 0] : [dx, dy];
  const { x, y, z } = pad.view;
  setView(x + px / z, y + py / z, z);
}

const navPair = () => [...nav.pts.values()].slice(0, 2);
const centreOf = (pts) => (pts.length > 1 ? mid(pts[0], pts[1]) : pts[0]);
const spreadOf = (pts) => (pts.length > 1 ? Math.hypot(pts[0][0] - pts[1][0], pts[0][1] - pts[1][1]) : 0);

/** Remembers the board point under the navigating pointers (their centre) and how far apart they are. */
function anchorNav() {
  const pts = navPair();
  nav.start = pts.length ? { world: toWorld(centreOf(pts)), d: spreadOf(pts), z: pad.view.z } : null;
}

/** One pointer drags the board; two pinch it, the point between them staying under them. */
function moveNav() {
  const s = nav.start;
  const pts = navPair();
  if (!s || !pts.length) return;
  const z = pts.length > 1 && s.d > 0 ? clampZ((s.z * spreadOf(pts)) / s.d) : s.z;
  const [mx, my] = centreOf(pts);
  setView(s.world[0] - mx / z, s.world[1] - my / z, z);
}

// ---------- erasing and history ----------

function nearSegment(px, py, a, b) {
  const dx = b[0] - a[0];
  const dy = b[1] - a[1];
  const len = dx * dx + dy * dy;
  const t = len ? Math.max(0, Math.min(1, ((px - a[0]) * dx + (py - a[1]) * dy) / len)) : 0;
  return Math.hypot(px - (a[0] + t * dx), py - (a[1] + t * dy));
}

function hits(stroke, x, y, radius) {
  const reach = radius + (stroke.w * 1.65) / 2;
  const p = stroke.p;
  if (p.length === 1) return Math.hypot(x - p[0][0], y - p[0][1]) <= reach;
  for (let i = 0; i + 1 < p.length; i++) if (nearSegment(x, y, p[i], p[i + 1]) <= reach) return true;
  return false;
}

/** Erases what the eraser touches moving to (x, y), including the ground since its last position. */
function eraseTo(x, y) {
  const [fromX, fromY] = active.last ?? [x, y];
  active.last = [x, y];
  const radius = active.radius;
  const steps = Math.max(1, Math.ceil(Math.hypot(x - fromX, y - fromY) / (radius / 2)));
  const gone = new Set();
  for (let i = 1; i <= steps; i++) {
    const px = fromX + ((x - fromX) * i) / steps;
    const py = fromY + ((y - fromY) * i) / steps;
    for (const s of pad.strokes) if (!gone.has(s) && hits(s, px, py, radius)) gone.add(s);
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

function accepts(e, prefs) {
  if (e.pointerType === 'pen') return true;
  if (e.pointerType === 'mouse') return prefs.inputs !== 'pen' && e.button === 0;
  if (e.pointerType === 'touch') return prefs.inputs === 'all';
  return false;
}

/** A pointer that moves the view instead of drawing: a finger that doesn't draw, a second finger, the middle button, a mouse that doesn't draw. */
function navigates(e, prefs) {
  if (e.pointerType === 'touch') return nav.pts.size > 0 || prefs.inputs !== 'all';
  if (e.pointerType === 'mouse') return e.button === 1 || (e.button === 0 && prefs.inputs === 'pen');
  return false;
}

function pressureOf(e, prefs) {
  return e.pointerType === 'pen' && prefs.pressure && e.pressure > 0 ? e.pressure : 0.5;
}

function capture(e) {
  try {
    els.canvas.setPointerCapture(e.pointerId);
  } catch {
    // The pointer is already gone; the gesture still follows the events it sends.
  }
}

function onDown(e) {
  if (!pad?.view) return;
  const prefs = penPrefs();
  const touch = e.pointerType === 'touch';
  if (touch && prefs.palm && performance.now() - lastPenAt < PALM_MS) return;
  if (active) {
    // A second finger turns a finger's stroke into a pinch; anything else waits for the stroke to end.
    if (!touch || !active.touch) return;
    const first = active;
    endGesture(true);
    nav.pts.set(first.id, first.at);
  } else if (nav.pts.size && !touch) {
    return;
  }
  if (navigates(e, prefs)) {
    e.preventDefault();
    capture(e);
    nav.pts.set(e.pointerId, stagePoint(e));
    anchorNav();
    return;
  }
  if (!accepts(e, prefs)) return;
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
  capture(e);
  const at = stagePoint(e);
  const [x, y] = toWorld(at);
  const z = pad.view.z;
  // The eraser's reach and the finest step a stroke keeps are sizes on screen, whatever the zoom.
  active = { id: e.pointerId, touch, at, tool, stroke: null, removed: [], prefs, radius: ERASER_RADIUS / z, step: 0.8 / z };
  els.pad.classList.toggle('erasing', tool === 'erase');
  if (tool === 'erase') {
    eraseTo(x, y);
    return;
  }
  const stroke = { id: pad.nextId++, c: ui.color, w: ui.width / z, p: [[x, y, pressureOf(e, prefs)]] };
  active.stroke = stroke;
  pad.strokes.push(stroke);
  ctx.lineCap = 'round';
  drawDot(ctx, stroke, palette());
}

function onMove(e) {
  if (nav.pts.has(e.pointerId)) {
    nav.pts.set(e.pointerId, stagePoint(e));
    moveNav();
    return;
  }
  if (!active || e.pointerId !== active.id) return;
  const r = els.canvas.getBoundingClientRect();
  active.at = stagePoint(e, r);
  const events = e.getCoalescedEvents?.() ?? [e];
  for (const ev of events.length ? events : [e]) {
    const [x, y] = toWorld(stagePoint(ev, r));
    if (active.tool === 'erase') {
      eraseTo(x, y);
      continue;
    }
    const s = active.stroke;
    const last = s.p[s.p.length - 1];
    if (Math.hypot(x - last[0], y - last[1]) < active.step) continue;
    s.p.push([x, y, pressureOf(ev, active.prefs)]);
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    drawSegment(ctx, s, s.p.length - 2, palette());
  }
}

function onUp(e, cancelled) {
  if (nav.pts.delete(e.pointerId)) {
    anchorNav();
    return;
  }
  if (active && e.pointerId === active.id) endGesture(cancelled);
}

function stopNav() {
  nav.pts.clear();
  nav.start = null;
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

/** The figure's SVG, sanitised, with its own size; given a viewBox when it has none so it scales instead of cropping. */
function figureSvg(fig) {
  const box = document.createElement('div');
  box.innerHTML = cleanSvg(fig.svg);
  const svg = box.querySelector('svg');
  if (!svg) return null;
  const vb = svg.viewBox?.baseVal;
  const w = (vb && vb.width) || parseFloat(svg.getAttribute('width')) || 300;
  const h = (vb && vb.height) || parseFloat(svg.getAttribute('height')) || 150;
  if (!svg.getAttribute('viewBox')) svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
  return { svg, w, h };
}

/** `size` scaled to fit inside `box`, centred. */
function fitRect(size, box) {
  const k = Math.min(box.w / size.w, box.h / size.h);
  return { x: box.x + (box.w - size.w * k) / 2, y: box.y + (box.h - size.h * k) / 2, w: size.w * k, h: size.h * k };
}

function renderBackground() {
  const layer = els.bgLayer;
  layer.replaceChildren();
  bgSize = null;
  const fig = currentFigure();
  const parsed = fig && figureSvg(fig);
  if (!parsed) return;
  const { svg } = parsed;
  svg.removeAttribute('width');
  svg.removeAttribute('height');
  svg.removeAttribute('role');
  svg.removeAttribute('aria-label');
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
  layer.append(svg);
  bgSize = { w: parsed.w, h: parsed.h };
  placeBackground();
}

/**
 * Puts the figure's layer where its board rectangle is on screen. A figure
 * without one yet is placed first: filling the old board for a pad saved
 * before zooming, otherwise most of what the learner is looking at.
 */
function placeBackground() {
  const layer = els.bgLayer;
  if (!pad?.view || !bgSize || !layer.firstChild) return;
  const { x, y, z } = pad.view;
  if (!pad.bgRect) {
    const [w, h] = stageSize();
    if (w < 40 || h < 40) return;
    const seen = { x: x + (0.05 * w) / z, y: y + (0.05 * h) / z, w: (0.9 * w) / z, h: (0.9 * h) / z };
    pad.bgRect = fitRect(bgSize, pad.bgFrame ?? seen);
    pad.bgFrame = null;
    scheduleSave();
  }
  const r = pad.bgRect;
  layer.style.left = `${(r.x - x) * z}px`;
  layer.style.top = `${(r.y - y) * z}px`;
  layer.style.width = `${r.w * z}px`;
  layer.style.height = `${r.h * z}px`;
}

/** Rebuilds the "Draw on" list from the session's figures; hides it when there are none. */
function refreshFigures() {
  if (!els) return;
  const list = figures();
  if (pad?.bg && !list.some((f) => f.id === pad.bg)) {
    pad.bg = null;
    pad.bgRect = null;
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

/** The background drawn into `c` (the export, already scaled by `s` and moved to the drawing's box) at its board rectangle. */
async function paintBackground(c, s) {
  const fig = currentFigure();
  const r = pad.bgRect;
  const parsed = fig && r && figureSvg(fig);
  if (!parsed) return;
  const { svg } = parsed;
  // Rasterised at the size it lands at, not scaled up from its own size.
  svg.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
  svg.setAttribute('width', String(Math.max(1, Math.round(r.w * s))));
  svg.setAttribute('height', String(Math.max(1, Math.round(r.h * s))));
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
  const xml = new XMLSerializer().serializeToString(svg).replace(/currentColor/gi, EXPORT.ink);
  const img = new Image();
  img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(xml)}`;
  await img.decode();
  c.drawImage(img, r.x, r.y, r.w, r.h);
}

/**
 * The drawing as a PNG data URL: dark ink on white, the figure underneath.
 * The picture is the drawing's bounding box with a white margin, scaled so
 * its long side is EXPORT_LONG px, whatever the zoom it was drawn at.
 */
async function exportPng() {
  const box = contentBox();
  const scale = (EXPORT_LONG - 2 * EXPORT_PAD) / Math.max(box.w, box.h, 1e-9);
  const canvas = document.createElement('canvas');
  canvas.width = Math.ceil(box.w * scale) + 2 * EXPORT_PAD;
  canvas.height = Math.ceil(box.h * scale) + 2 * EXPORT_PAD;
  const c = canvas.getContext('2d');
  c.fillStyle = EXPORT.paper;
  c.fillRect(0, 0, canvas.width, canvas.height);
  c.setTransform(scale, 0, 0, scale, EXPORT_PAD - box.x * scale, EXPORT_PAD - box.y * scale);
  try {
    await paintBackground(c, scale);
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
  stopNav();
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
  if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
  const key = e.key.toLowerCase();
  if (key === 'z' && !e.shiftKey && !field) {
    e.preventDefault();
    undo();
  } else if (key === '=' || key === '+') {
    e.preventDefault();
    zoomBy(ZOOM_STEP);
  } else if (key === '-' || key === '_') {
    e.preventDefault();
    zoomBy(1 / ZOOM_STEP);
  } else if (key === '0') {
    e.preventDefault();
    zoomToFit();
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
    zoomOut: $('pad-zoom-out'),
    zoomFit: $('pad-zoom-fit'),
    zoomIn: $('pad-zoom-in'),
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
  els.zoomOut.addEventListener('click', () => zoomBy(1 / ZOOM_STEP));
  els.zoomIn.addEventListener('click', () => zoomBy(ZOOM_STEP));
  els.zoomFit.addEventListener('click', zoomToFit);
  els.bg.addEventListener('change', () => {
    pad.bg = els.bg.value || null;
    pad.bgRect = null;
    pad.bgFrame = null;
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
  els.canvas.addEventListener('pointerup', (e) => onUp(e, false));
  els.canvas.addEventListener('pointercancel', (e) => onUp(e, true));
  els.canvas.addEventListener('lostpointercapture', (e) => onUp(e, false));
  els.canvas.addEventListener('wheel', onWheel, { passive: false });
  // Safari and WKWebView report a trackpad pinch as gesture events, not as Ctrl+wheel.
  els.canvas.addEventListener('gesturestart', (e) => {
    e.preventDefault();
    gestureZ = pad?.view?.z ?? 1;
  });
  els.canvas.addEventListener('gesturechange', (e) => {
    e.preventDefault();
    zoomTo(gestureZ * e.scale, stagePoint(e));
  });

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
