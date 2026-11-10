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

/** "Learn · 10:42", "Practice · Tue", "Read-only · 1 Jan", "Locked · 10:42" (desktop-only plugin, on a phone). */
function metaText(s) {
  const kind = isLocked(s) ? 'Locked' : s.readOnly ? 'Read-only' : s.mode === 'practice' ? 'Practice' : 'Learn';
  const t = time(s.updatedAt);
  const today = startOfToday();
  const when = t >= today ? clock.format(t) : t >= today - 7 * DAY ? weekday.format(t) : dayMonth.format(t);
  return `${kind} · ${when}`;
}

/** Read-only on this phone because the session used a desktop-only plugin (docs/sync-build.md, "Sessions on mobile"). */
const isLocked = (s) => s.readOnly && s.readOnlyReason === 'desktop-plugin';

/** "Extract" item note: why it is greyed, or how much it would read. */
function extractNote(st) {
  if (st.running) return 'Running now';
  const n = st.uncoveredTokens.toLocaleString();
  if (st.uncoveredTokens < st.manualTokens) return `${n} of ${st.manualTokens.toLocaleString()} new tokens needed`;
  return `${n} new tokens`;
}

/**
 * @param {HTMLElement} root   container for the groups
 * @param {{
 *   onOpen(id: string): void,
 *   onDelete(id: string): Promise<void>,
 *   onExtractStatus(id: string): Promise<{ uncoveredTokens: number, manualTokens: number, running: boolean }>,
 *   onExtract(id: string): Promise<void>,
 * }} hooks
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
      <div class="session-more-wrap">
        <button class="btn text session-more" type="button" aria-haspopup="menu" aria-expanded="false">…</button>
        <div class="menu session-menu" role="menu" hidden>
          <button class="menu-item session-extract" type="button" role="menuitem" disabled>Extract<span class="menu-note"></span></button>
          <button class="menu-item session-delete" type="button" role="menuitem" aria-live="polite">Delete</button>
        </div>
      </div>
      <p class="session-status" role="status"></p>`;
    fillRow(li, s);
    return li;
  }

  function fillRow(li, s) {
    li.querySelector('.session-title').textContent = s.title || 'Untitled';
    li.querySelector('.session-meta').textContent = metaText(s);
    // Dimmed but still openable: the session shows its lock note and "Continue here anyway".
    li.classList.toggle('locked', isLocked(s));
    li.querySelector('.session-more').setAttribute('aria-label', `More actions for ${s.title || 'this session'}`);
  }

  // ---------- the row's "…" menu ----------

  let openMenu = null; // the li whose menu is open

  const menuItems = (li) => [...li.querySelectorAll('.session-menu [role="menuitem"]')].filter((b) => !b.disabled);

  function disarm(del) {
    clearTimeout(del._disarm);
    del.classList.remove('armed');
    del.textContent = 'Delete';
  }

  function closeMenu({ restoreFocus = false } = {}) {
    const li = openMenu;
    if (!li) return;
    openMenu = null;
    li.classList.remove('menu-open');
    li.querySelector('.session-menu').hidden = true;
    const more = li.querySelector('.session-more');
    more.setAttribute('aria-expanded', 'false');
    disarm(li.querySelector('.session-delete'));
    if (restoreFocus) more.focus({ preventScroll: true });
  }

  /** Opens a row's menu; Extract stays greyed until its status says it may run. */
  function openRowMenu(li, { focusFirst = false } = {}) {
    closeMenu();
    openMenu = li;
    li.classList.add('menu-open');
    li.querySelector('.session-menu').hidden = false;
    li.querySelector('.session-more').setAttribute('aria-expanded', 'true');
    const extract = li.querySelector('.session-extract');
    const note = extract.querySelector('.menu-note');
    extract.disabled = true;
    note.textContent = 'Checking…';
    if (focusFirst) menuItems(li)[0]?.focus({ preventScroll: true });
    hooks
      .onExtractStatus(li.dataset.id)
      .then((st) => {
        if (openMenu !== li) return;
        note.textContent = extractNote(st);
        extract.disabled = st.running || st.uncoveredTokens < st.manualTokens;
      })
      .catch((err) => {
        if (openMenu === li) note.textContent = `Unavailable: ${err}`;
      });
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
    openMenu = null;
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

  function setRowStatus(id, text, { error = true } = {}) {
    const p = rows.get(id)?.querySelector('.session-status');
    if (!p) return;
    p.textContent = text;
    p.classList.toggle('note', !error);
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
    if (e.target.closest('.session-more')) {
      if (openMenu === li) closeMenu();
      else openRowMenu(li);
      return;
    }
    if (e.target.closest('.session-extract')) {
      closeMenu({ restoreFocus: true });
      setRowStatus(id, 'Extracting into the map…', { error: false });
      hooks.onExtract(id).then(
        () => setRowStatus(id, 'Extracting into the map in the background.', { error: false }),
        (err) => setRowStatus(id, `Couldn’t extract this session: ${err}`),
      );
      return;
    }
    const del = e.target.closest('.session-delete');
    if (!del) return;
    // Two presses: the first arms the item for a few seconds.
    if (!del.classList.contains('armed')) {
      del.classList.add('armed');
      del.textContent = 'Delete? Press again';
      clearTimeout(del._disarm);
      del._disarm = setTimeout(() => disarm(del), ARM_MS);
      return;
    }
    closeMenu();
    hooks.onDelete(id).catch((err) => {
      setRowStatus(id, `Couldn’t delete this session: ${err}`);
    });
  });

  root.addEventListener('keydown', (e) => {
    const li = e.target.closest('.session');
    if (!li) return;
    if (e.target.closest('.session-more')) {
      if (e.key === 'ArrowDown' || ((e.key === 'Enter' || e.key === ' ') && openMenu !== li)) {
        e.preventDefault();
        openRowMenu(li, { focusFirst: true });
      } else if (e.key === 'Escape') {
        closeMenu();
      }
      return;
    }
    if (!e.target.closest('.session-menu')) return;
    const items = menuItems(li);
    const index = items.indexOf(document.activeElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const next = e.key === 'ArrowDown' ? (index + 1) % items.length : (index - 1 + items.length) % items.length;
      items[next]?.focus({ preventScroll: true });
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      items[e.key === 'Home' ? 0 : items.length - 1]?.focus({ preventScroll: true });
    } else if (e.key === 'Escape') {
      e.preventDefault();
      closeMenu({ restoreFocus: true });
    } else if (e.key === 'Tab') {
      closeMenu();
    }
  });

  document.addEventListener('click', (e) => {
    if (openMenu && !openMenu.querySelector('.session-more-wrap').contains(e.target)) closeMenu();
  });

  return { render, upsert, bump, remove, reset, has, get, newest, newestLearn, openButton, setRowStatus };
}
