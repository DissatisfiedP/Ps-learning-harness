// Pen preferences (Settings → Pen) and how a pointer event's `buttons` mask
// reads as pen buttons. UI-only, kept per device in localStorage like the
// theme and focus choices; the scratch pad reads them on every stroke.

const KEY = 'lh.pen';
const SEEN_KEY = 'lh.pen.seen';

/** What a pen button can do. `erase` is a hold: it erases while the button is down. */
export const BUTTON_ACTIONS = [
  { value: 'none', label: 'Nothing' },
  { value: 'erase', label: 'Erase while held' },
  { value: 'undo', label: 'Undo' },
  { value: 'color', label: 'Next colour' },
  { value: 'width', label: 'Next line width' },
  { value: 'pad', label: 'Show or hide the scratch pad' },
];

export const INPUTS = ['pen', 'mouse', 'all'];

const DEFAULTS = {
  inputs: 'mouse',
  pressure: true,
  palm: true,
  barrel: 'erase',
  barrel2: 'undo',
  eraser: 'erase',
};

const BUTTON_NAMES = { barrel: 'barrel button', barrel2: 'second barrel button', eraser: 'eraser end' };

/** `PointerEvent.buttons` bits: 1 tip or left, 2 barrel, 4 second barrel (middle), 32 eraser. */
export function penButtons(buttons) {
  const b = Number(buttons) || 0;
  return { tip: Boolean(b & 1), barrel: Boolean(b & 2), barrel2: Boolean(b & 4), eraser: Boolean(b & 32) };
}

/** Where the choices live when localStorage is unavailable. */
let fallback = {};

function read() {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) ?? '{}');
    return raw && typeof raw === 'object' ? raw : {};
  } catch {
    return fallback;
  }
}

/** The saved pen choices over the defaults; anything unrecognised falls back. */
export function penPrefs() {
  const saved = read();
  const actions = BUTTON_ACTIONS.map((a) => a.value);
  const out = { ...DEFAULTS };
  if (INPUTS.includes(saved.inputs)) out.inputs = saved.inputs;
  for (const k of ['pressure', 'palm']) if (typeof saved[k] === 'boolean') out[k] = saved[k];
  for (const k of ['barrel', 'barrel2', 'eraser']) if (actions.includes(saved[k])) out[k] = saved[k];
  return out;
}

export function setPenPref(name, value) {
  const next = { ...read(), [name]: value };
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    // Not persisted; the choice still applies until the window closes.
    fallback = next;
  }
}

/** Whether a pen has touched or hovered over this device's screen. */
export function penSeen() {
  try {
    return localStorage.getItem(SEEN_KEY) === '1';
  } catch {
    return false;
  }
}

/** Notes the first pen the page sees, so Settings can say one was found. */
export function watchForPen() {
  if (penSeen()) return;
  const onPen = (e) => {
    if (e.pointerType !== 'pen') return;
    document.removeEventListener('pointermove', onPen, true);
    document.removeEventListener('pointerdown', onPen, true);
    try {
      localStorage.setItem(SEEN_KEY, '1');
    } catch {
      // Nothing to remember it with.
    }
    renderDetected();
  };
  document.addEventListener('pointermove', onPen, true);
  document.addEventListener('pointerdown', onPen, true);
}

// ---------- Settings → Pen ----------

function renderDetected() {
  const el = document.getElementById('pen-detected');
  if (!el) return;
  el.textContent = penSeen()
    ? 'A pen has been used on this device. These settings apply to the scratch pad in a session.'
    : 'No pen seen on this device yet. Use the box below to try yours. These settings apply to the scratch pad in a session.';
}

/** Describes one pen event for the test box. */
function describe(e, seen) {
  const b = penButtons(e.buttons);
  const held = [b.barrel && BUTTON_NAMES.barrel, b.barrel2 && BUTTON_NAMES.barrel2, b.eraser && BUTTON_NAMES.eraser]
    .filter(Boolean);
  for (const name of held) seen.add(name);
  const parts = [e.pointerType || 'unknown'];
  if (e.pointerType === 'pen') parts.push(`pressure ${e.pressure.toFixed(2)}`);
  if (e.pointerType === 'pen' && (e.tiltX || e.tiltY)) parts.push(`tilt ${Math.round(e.tiltX)}°, ${Math.round(e.tiltY)}°`);
  parts.push(b.tip ? 'touching' : 'hovering');
  if (held.length) parts.push(`${held.join(' + ')} down`);
  const known = seen.size ? ` Buttons seen so far: ${[...seen].join(', ')}.` : '';
  return `${parts.join(' · ')}.${known}`;
}

function wireTest() {
  const box = document.getElementById('pen-test');
  const readout = document.getElementById('pen-test-readout');
  if (!box || !readout) return;
  const seen = new Set();
  const canvas = document.createElement('canvas');
  canvas.className = 'pen-test-canvas';
  canvas.setAttribute('aria-hidden', 'true');
  box.append(canvas);
  const ctx = canvas.getContext('2d');
  let drawing = false;
  const fit = () => {
    const r = box.getBoundingClientRect();
    if (!r.width) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(r.width * dpr);
    canvas.height = Math.round(r.height * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  };
  const at = (e) => {
    const r = canvas.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  };
  const ink = () => getComputedStyle(box).color;
  box.addEventListener('pointerdown', (e) => {
    if (!canvas.width) fit();
    if (e.pointerType === 'pen' || e.pointerType === 'mouse') box.setPointerCapture(e.pointerId);
    drawing = Boolean(e.buttons & 1) || Boolean(e.buttons & 32);
    readout.textContent = describe(e, seen);
    if (drawing) {
      const [x, y] = at(e);
      ctx.beginPath();
      ctx.moveTo(x, y);
    }
  });
  box.addEventListener('pointermove', (e) => {
    readout.textContent = describe(e, seen);
    if (!drawing) return;
    const [x, y] = at(e);
    ctx.strokeStyle = ink();
    ctx.lineCap = 'round';
    ctx.lineWidth = 1 + (e.pointerType === 'pen' ? e.pressure : 0.5) * 5;
    ctx.lineTo(x, y);
    ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(x, y);
  });
  const end = () => {
    drawing = false;
  };
  box.addEventListener('pointerup', end);
  box.addEventListener('pointercancel', end);
  // A pen's barrel button opens a context menu on some systems.
  box.addEventListener('contextmenu', (e) => e.preventDefault());
  box.addEventListener('dblclick', () => ctx.clearRect(0, 0, canvas.width, canvas.height));
  new ResizeObserver(fit).observe(box);
}

export function initPenSettings() {
  const inputs = document.getElementById('pen-inputs');
  const toggles = document.getElementById('pen-toggles');
  if (!inputs || !toggles) return;
  const prefs = penPrefs();
  for (const r of inputs.querySelectorAll('input')) {
    r.checked = r.value === prefs.inputs;
    r.addEventListener('change', () => r.checked && setPenPref('inputs', r.value));
  }
  for (const box of toggles.querySelectorAll('input[data-pen]')) {
    box.checked = prefs[box.dataset.pen];
    box.addEventListener('change', () => setPenPref(box.dataset.pen, box.checked));
  }
  for (const select of document.querySelectorAll('select[data-pen-button]')) {
    for (const a of BUTTON_ACTIONS) select.append(new Option(a.label, a.value));
    select.value = prefs[select.dataset.penButton];
    select.addEventListener('change', () => setPenPref(select.dataset.penButton, select.value));
  }
  renderDetected();
  wireTest();
  watchForPen();
}
