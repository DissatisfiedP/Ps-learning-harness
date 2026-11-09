// Collapsible Settings sections (ui-design §7.5): the section's h2 holds one
// button with `aria-expanded`/`aria-controls` that shows or hides the body,
// plus a one-line summary that stays useful while closed. Closed by default;
// the learner's own open/close is kept per device in
// `localStorage["lh.settings.<name>"]` (`"open"`/`"closed"`). Expanding a
// section to navigate into it is for that visit only and changes nothing stored.

const sections = new Map(); // name -> { button, body, summary }

const key = (name) => `lh.settings.${name}`;

function stored(name) {
  try {
    return localStorage.getItem(key(name)) === 'open';
  } catch {
    return false;
  }
}

function store(name, open) {
  try {
    localStorage.setItem(key(name), open ? 'open' : 'closed');
  } catch {
    // Storage can be unavailable; the section still toggles for this visit.
  }
}

function show(s, open) {
  s.button.setAttribute('aria-expanded', String(open));
  s.body.hidden = !open;
}

/** The section `name` (`#<name>-toggle`, `#<name>-body`, `#<name>-summary`), wired on first use. */
function section(name) {
  let s = sections.get(name);
  if (s) return s;
  s = {
    button: document.getElementById(`${name}-toggle`),
    body: document.getElementById(`${name}-body`),
    summary: document.getElementById(`${name}-summary`),
  };
  show(s, stored(name));
  s.button.addEventListener('click', () => {
    const open = s.body.hidden;
    show(s, open);
    store(name, open);
  });
  sections.set(name, s);
  return s;
}

/** Wires each named section in its remembered state. */
export function initSections(...names) {
  for (const name of names) section(name);
}

/** Puts every wired section back to what this device remembers (on each Settings open). */
export function restoreSections() {
  for (const [name, s] of sections) show(s, stored(name));
}

export function setSectionSummary(name, text) {
  section(name).summary.textContent = text;
}

/** Opens `name` for this visit and returns its heading button, to scroll to or focus. */
export function expandSection(name) {
  const s = section(name);
  show(s, true);
  return s.button;
}
