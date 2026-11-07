// The Home screen's session list. Rendered in full once at startup; after
// that each change (new session, retitle, a finished turn, a delete) moves or
// updates only its own row. No re-listing.

const GROUPS = ['Today', 'Yesterday', 'Previous 7 days', 'Older'];
const DAY = 24 * 60 * 60 * 1000;
const ARM_MS = 4000;
const EMPTY = 'No sessions yet.';

const time = (iso) => Date.parse(iso) || 0;

function startOfToday() {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

function groupLabel(iso) {
  const t = time(iso);
  const today = startOfToday();
  if (t >= today) return 'Today';
  if (t >= today - DAY) return 'Yesterday';
  if (t >= today - 7 * DAY) return 'Previous 7 days';
  return 'Older';
}

const clock = new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit' });
const weekday = new Intl.DateTimeFormat(undefined, { weekday: 'short' });
const dayMonth = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short' });

/** "Learn · 10:42", "Practice · Tue", "Read-only · 1 Jan". */
function metaText(s) {
  const kind = s.readOnly ? 'Read-only' : s.mode === 'practice' ? 'Practice' : 'Learn';
  const t = time(s.updatedAt);
  const today = startOfToday();
  const when = t >= today ? clock.format(t) : t >= today - 7 * DAY ? weekday.format(t) : dayMonth.format(t);
  return `${kind} · ${when}`;
}

/**
 * @param {HTMLElement} root   container for the groups
 * @param {{ onOpen(id: string): void, onDelete(id: string): Promise<void> }} hooks
 */
export function createSessionList(root, hooks) {
  let sessions = []; // SessionSummary, newest first
  const rows = new Map(); // id -> li

  function rowEl(s) {
    const li = document.createElement('li');
    li.className = 'session';
    li.dataset.id = s.id;
    li.innerHTML = `
      <button class="session-open" type="button"><span class="session-title"></span><span class="session-meta"></span></button>
      <button class="btn text session-delete" type="button" aria-live="polite">Delete</button>
      <p class="session-status" role="status"></p>`;
    fillRow(li, s);
    return li;
  }

  function fillRow(li, s) {
    li.querySelector('.session-title').textContent = s.title || 'Untitled';
    li.querySelector('.session-meta').textContent = metaText(s);
    li.querySelector('.session-delete').setAttribute('aria-label', `Delete ${s.title || 'session'}`);
  }

  function groupEl(label, create) {
    let g = root.querySelector(`.session-group[data-group="${label}"]`);
    if (g || !create) return g;
    g = document.createElement('div');
    g.className = 'session-group';
    g.dataset.group = label;
    g.innerHTML = `<h3 class="small-caps">${label}</h3><ul></ul>`;
    const order = GROUPS.indexOf(label);
    const after = [...root.querySelectorAll('.session-group')].find((x) => GROUPS.indexOf(x.dataset.group) > order);
    root.querySelector('.session-empty')?.remove();
    root.insertBefore(g, after ?? null);
    return g;
  }

  function prune() {
    for (const g of root.querySelectorAll('.session-group')) {
      if (!g.querySelector('.session')) g.remove();
    }
    if (!sessions.length && !root.querySelector('.session-empty')) {
      root.innerHTML = `<p class="session-empty">${EMPTY}</p>`;
    }
  }

  function render() {
    rows.clear();
    root.textContent = '';
    if (!sessions.length) {
      root.innerHTML = `<p class="session-empty">${EMPTY}</p>`;
      return;
    }
    const frag = document.createDocumentFragment();
    let label = null;
    let ul = null;
    for (const s of sessions) {
      const l = groupLabel(s.updatedAt);
      if (l !== label) {
        label = l;
        const g = document.createElement('div');
        g.className = 'session-group';
        g.dataset.group = l;
        g.innerHTML = `<h3 class="small-caps">${l}</h3><ul></ul>`;
        ul = g.lastChild;
        frag.append(g);
      }
      const li = rowEl(s);
      rows.set(s.id, li);
      ul.append(li);
    }
    root.append(frag);
  }

  /** Inserts or updates one session, keeping newest-first order. */
  function upsert(summary) {
    const i = sessions.findIndex((x) => x.id === summary.id);
    const next = i >= 0 ? { ...sessions[i], ...summary } : { ...summary };
    next.updatedAt ??= next.createdAt ?? new Date().toISOString();
    if (i >= 0) sessions.splice(i, 1);
    const t = time(next.updatedAt);
    let j = sessions.findIndex((x) => time(x.updatedAt) <= t);
    if (j < 0) j = sessions.length;
    sessions.splice(j, 0, next);

    let li = rows.get(next.id);
    if (li) fillRow(li, next);
    else {
      li = rowEl(next);
      rows.set(next.id, li);
    }
    const label = groupLabel(next.updatedAt);
    const ul = groupEl(label, true).querySelector('ul');
    const after = sessions[j + 1];
    const anchor = after && groupLabel(after.updatedAt) === label ? rows.get(after.id) : null;
    if (li.parentNode !== ul || li.nextSibling !== (anchor ?? null)) ul.insertBefore(li, anchor ?? null);
    prune();
  }

  /** Marks a session as just updated (after a turn) without re-listing. */
  function bump(id) {
    if (sessions.some((x) => x.id === id)) upsert({ id, updatedAt: new Date().toISOString() });
  }

  /** Removes a row; returns the open button focus should move to, if any. */
  function remove(id) {
    const li = rows.get(id);
    const order = [...root.querySelectorAll('.session')];
    const k = order.indexOf(li);
    const neighbour = order[k + 1] ?? order[k - 1] ?? null;
    sessions = sessions.filter((x) => x.id !== id);
    li?.remove();
    rows.delete(id);
    prune();
    return neighbour?.querySelector('.session-open') ?? null;
  }

  /** Replaces the list with `list` (startup), keeping rows added meanwhile. */
  function reset(list) {
    const seen = new Set(list.map((x) => x.id));
    sessions = [...sessions.filter((x) => !seen.has(x.id)), ...list].sort((a, b) => time(b.updatedAt) - time(a.updatedAt));
    render();
  }

  const has = (id) => sessions.some((x) => x.id === id);
  const get = (id) => sessions.find((x) => x.id === id) ?? null;
  const newest = () => sessions[0] ?? null;
  /** The newest Learn session that can still be read for state (not practice, not read-only). */
  const newestLearn = () => sessions.find((x) => x.mode === 'learn' && !x.readOnly) ?? null;
  const openButton = (id) => rows.get(id)?.querySelector('.session-open') ?? null;

  function setRowStatus(id, text) {
    const p = rows.get(id)?.querySelector('.session-status');
    if (p) p.textContent = text;
  }

  root.addEventListener('click', (e) => {
    const li = e.target.closest('.session');
    if (!li) return;
    const id = li.dataset.id;
    if (e.target.closest('.session-open')) {
      setRowStatus(id, '');
      hooks.onOpen(id);
      return;
    }
    const del = e.target.closest('.session-delete');
    if (!del) return;
    // Two presses: the first arms the button for a few seconds.
    if (!del.classList.contains('armed')) {
      del.classList.add('armed');
      del.textContent = 'Delete? Press again';
      clearTimeout(del._disarm);
      del._disarm = setTimeout(() => {
        del.classList.remove('armed');
        del.textContent = 'Delete';
      }, ARM_MS);
      return;
    }
    clearTimeout(del._disarm);
    hooks.onDelete(id).catch((err) => {
      del.classList.remove('armed');
      del.textContent = 'Delete';
      setRowStatus(id, `Couldn’t delete this session: ${err}`);
    });
  });

  return { render, upsert, bump, remove, reset, has, get, newest, newestLearn, openButton, setRowStatus };
}
