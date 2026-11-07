// The field guide and the confidence lines (docs/learning-system.md §6, §7;
// docs/ui-design.md §7.7.5–§7.7.6).
//
// Venues are a property of **topics** in the knowledge graph, so the Map shows
// them on a topic's card and in its own list. The one session-scoped use left
// is the close's "useful now": the tutor judges which of the session's brief
// venues the learner can use right now, and the split below is how it says so.
// Both are reference material, so both hang off closed disclosures. Nothing
// here decides anything: the numbers and the words come from the backend.

const h = (tag, className, text) => {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text !== undefined) el.textContent = text;
  return el;
};

const KIND_LABEL = { ask: 'Ask', discussion: 'Discussion', reference: 'Reference' };
const KIND_ORDER = ['ask', 'discussion', 'reference'];

const shortUrl = (url) => {
  try {
    const u = new URL(url);
    return `${u.host}${u.pathname === '/' ? '' : u.pathname}`;
  } catch {
    return url;
  }
};

/** One venue: a link (or its name), then the note that says why it's worth it. */
function venueLine(v, onLink, { entry = false } = {}) {
  const li = h('li', 'venue');
  const name = v.name ?? v.url ?? 'Venue';
  if (v.url) {
    const a = h('a', 'venue-name', name);
    a.href = v.url;
    a.rel = 'noreferrer';
    a.title = shortUrl(v.url);
    if (onLink) a.addEventListener('click', (e) => {
      e.preventDefault();
      onLink(v.url);
    });
    li.append(a);
  } else {
    li.append(h('span', 'venue-name', name));
  }
  if (v.kind && KIND_LABEL[v.kind]) li.append(h('span', 'venue-kind', KIND_LABEL[v.kind]));
  if (v.note) li.append(h('span', 'venue-note', ` — ${v.note}`));
  // On a topic's card the entry level is what says when the venue starts to be
  // readable; asking works at every level, so it is not worth a line there.
  const level = (v.entryLevel ?? '').trim();
  if (entry && v.kind !== 'ask' && level && level.toLowerCase() !== 'anyone') {
    li.append(h('span', 'venue-unlock', ` — after ${level}`));
  }
  return li;
}

/**
 * Venues useful now and those coming up, for the **close** only.
 *
 * The durable record lives on topics in the graph; the close is the one
 * session-scoped judgement left: which of *this session's* brief venues the
 * learner can use today. The brief does not say it, so the split follows the
 * one rule §6 states outright: asking works at every level, while discussion
 * and reference venues unlock with their `entryLevel`. A venue with no entry
 * level is not waiting on anything, so it counts as useful now; an explicit
 * `usefulNow`/`now` from the brief wins over both.
 */
export function splitVenues(venues = [], depth = '') {
  const usable = (v) => {
    if (typeof v.usefulNow === 'boolean') return v.usefulNow;
    if (typeof v.now === 'boolean') return v.now;
    if (v.kind === 'ask') return true;
    return !v.entryLevel;
  };
  const named = (v) => v && (v.name || v.url);
  let now = venues.filter((v) => named(v) && usable(v));
  let later = venues.filter((v) => named(v) && !usable(v));
  // `depth: "pass"` wants where to get help and nothing else (§6).
  if (depth === 'pass') {
    now = now.filter((v) => v.kind === 'ask');
    later = [];
  }
  return { now, later };
}

/**
 * A venue list grouped Ask / Discussion / Reference, in that order. The Map's
 * topic card and the map-side list use this; `fieldGuide` (below) groups the
 * same way behind its now/later split.
 */
export function venueGroups(list = [], onLink = null, opts = {}) {
  const frag = document.createDocumentFragment();
  for (const kind of KIND_ORDER) {
    const group = list.filter((v) => (v.kind ?? '') === kind);
    if (!group.length) continue;
    const wrap = h('div', 'venue-group');
    wrap.append(h('p', 'venue-head', KIND_LABEL[kind]));
    const ul = h('ul', 'venue-list');
    for (const v of group) ul.append(venueLine(v, onLink, opts));
    wrap.append(ul);
    frag.append(wrap);
  }
  const other = list.filter((v) => !KIND_ORDER.includes(v.kind ?? ''));
  if (other.length) {
    const ul = h('ul', 'venue-list');
    for (const v of other) ul.append(venueLine(v, onLink, opts));
    frag.append(ul);
  }
  return frag;
}

/**
 * "Where the field lives": useful now, then later with what unlocks each one.
 * Returns null when there is nothing to show, so callers stay harmless.
 */
export function fieldGuide(venues, { depth = '', onLink = null, open = false, heading = 'Where the field lives' } = {}) {
  const { now, later } = splitVenues(venues, depth);
  if (!now.length && !later.length) return null;
  const details = h('details', 'field-guide');
  details.open = open;
  details.append(h('summary', 'field-guide-title', heading));
  const body = h('div', 'field-guide-body');
  if (now.length) {
    body.append(h('p', 'field-guide-head', 'Useful now'));
    body.append(venueGroups(now, onLink));
  }
  if (later.length) {
    body.append(h('p', 'field-guide-head', 'Later'));
    const ul = h('ul', 'venue-list');
    for (const v of later) {
      const line = venueLine(v, onLink);
      if (v.entryLevel) line.append(h('span', 'venue-unlock', ` — after ${v.entryLevel}`));
      ul.append(line);
    }
    body.append(ul);
  }
  details.append(body);
  return details;
}

/** "Fairly sure: right 9 of 15" — one line per bucket the backend reported. */
export function calibrationLines(calibration) {
  const buckets = calibration?.buckets ?? [];
  return buckets.map((b) => `${b.label}: right ${b.correct} of ${b.total}`);
}

/** The close's "Your confidence" disclosure, or null below the threshold. */
export function calibrationBlock(calibration) {
  if (!calibration) return null;
  const lines = calibrationLines(calibration);
  const notes = (calibration.notes ?? []).filter(Boolean);
  if (!lines.length && !notes.length) return null;
  const details = h('details', 'calibration');
  details.append(h('summary', 'calibration-title', 'Your confidence'));
  const body = h('div', 'calibration-body');
  for (const line of lines) body.append(h('p', 'calibration-line', line));
  for (const note of notes) body.append(h('p', 'calibration-note', note));
  details.append(body);
  return details;
}

/** One line for the Map page, under the counts: "Fairly sure 9/15 · Certain 3/4". */
export function calibrationSummary(calibration) {
  const buckets = calibration?.buckets ?? [];
  if (!buckets.length) return '';
  return buckets.map((b) => `${b.label} ${b.correct}/${b.total}`).join(' · ');
}
