// Model search for the Settings role pickers: a combobox over one provider's
// model list. With nothing typed it shows the list in the provider's own
// order; typing ranks the matches. At most LIMIT options are rendered, so a
// provider listing hundreds of models stays as quick as one listing five.

const LIMIT = 10;
let seq = 0;

const SEPARATORS = /[\s/:._-]+/;
const squash = (s) => s.replace(new RegExp(SEPARATORS, 'g'), '');

/**
 * How well `model` matches `query`, 0 best; `null` when it doesn't match.
 * Both the id and the display name are searched, case-insensitively.
 */
export function rank(model, query) {
  const q = query.trim().toLowerCase();
  if (!q) return 0;
  const fields = [model.id.toLowerCase(), (model.name ?? '').toLowerCase()];
  if (fields.includes(q)) return 0;
  if (fields.some((f) => f.startsWith(q))) return 1;
  // A word start: "sonnet" in "claude-sonnet-4-6", "gpt" in "openai/gpt-5".
  if (fields.some((f) => f.split(SEPARATORS).some((w) => w.startsWith(q)))) return 2;
  if (fields.some((f) => f.includes(q))) return 3;
  const terms = q.split(/\s+/).filter(Boolean);
  const hay = fields.join(' ');
  if (terms.length > 1 && terms.every((t) => hay.includes(t))) return 4;
  // Separators ignored: "gpt5" finds "gpt-5", "opus 4.8" finds "claude-opus-4-8".
  const sq = squash(q);
  if (sq && fields.some((f) => squash(f).includes(sq))) return 5;
  return null;
}

/** The best `limit` matches in rank order (ties keep the provider's order), and how many matched. */
export function topMatches(list, query, limit = LIMIT) {
  if (!query.trim()) return { items: list.slice(0, limit), total: list.length };
  const scored = [];
  list.forEach((m, i) => {
    const r = rank(m, query);
    if (r !== null) scored.push([r, i, m]);
  });
  scored.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  return { items: scored.slice(0, limit).map((s) => s[2]), total: scored.length };
}

const contextLabel = (n) => (n ? `${n >= 1e6 ? `${+(n / 1e6).toFixed(1)}M` : `${Math.round(n / 1000)}K`} ctx` : '');

/**
 * Turns `input` into a model combobox. `models()` returns the current list,
 * or `null` while it is loading. `onPick(model)` runs after a choice. Free
 * text stays allowed: an id the provider didn't list is used as typed.
 */
export function modelSearch(input, { models, onPick }) {
  const listId = `model-list-${++seq}`;
  const pop = document.createElement('div');
  pop.className = 'model-pop';
  pop.hidden = true;
  const list = document.createElement('ul');
  list.className = 'mention-list';
  list.id = listId;
  list.setAttribute('role', 'listbox');
  list.setAttribute('aria-label', 'Models');
  pop.append(list);
  input.after(pop);
  input.setAttribute('role', 'combobox');
  input.setAttribute('aria-autocomplete', 'list');
  input.setAttribute('aria-controls', listId);
  input.setAttribute('aria-expanded', 'false');

  let open = false;
  let query = '';
  let items = [];
  let active = -1;

  const note = (text) => {
    const li = document.createElement('li');
    li.className = 'mention-empty';
    li.setAttribute('role', 'presentation');
    li.textContent = text;
    list.append(li);
  };

  const setActive = (index) => {
    if (!items.length) return;
    active = (index + items.length) % items.length;
    for (const [i, opt] of [...list.querySelectorAll('[role="option"]')].entries()) {
      opt.setAttribute('aria-selected', String(i === active));
      if (i === active) {
        input.setAttribute('aria-activedescendant', opt.id);
        opt.scrollIntoView({ block: 'nearest', behavior: 'instant' });
      }
    }
  };

  const render = () => {
    list.textContent = '';
    input.removeAttribute('aria-activedescendant');
    const all = models();
    items = [];
    active = -1;
    if (all === null) {
      note('Loading models…');
    } else {
      const { items: top, total } = topMatches(all, query);
      items = top;
      for (const [i, m] of top.entries()) {
        const opt = document.createElement('li');
        opt.id = `${listId}-${i}`;
        opt.className = 'mention-option';
        opt.setAttribute('role', 'option');
        opt.setAttribute('aria-selected', 'false');
        const name = document.createElement('span');
        name.className = 'mention-name';
        name.textContent = m.name || m.id;
        const kind = document.createElement('span');
        kind.className = 'mention-kind';
        kind.textContent = contextLabel(m.contextWindow);
        opt.append(name, kind);
        if (m.name && m.name !== m.id) {
          const id = document.createElement('span');
          id.className = 'mention-path';
          id.textContent = m.id;
          opt.append(id);
        }
        // mousedown keeps focus in the input; click picks.
        opt.addEventListener('mousedown', (e) => e.preventDefault());
        opt.addEventListener('mousemove', () => active !== i && setActive(i));
        opt.addEventListener('click', () => pick(i));
        list.append(opt);
      }
      if (!all.length) note('This provider listed no models. Type an id.');
      else if (!total) note('No model matches. The id you typed is used as is.');
      else if (total > top.length) {
        note(query.trim()
          ? `${total - top.length} more match. Keep typing to narrow.`
          : `First ${top.length} of ${total}. Type to search.`);
      }
      // Nothing typed: highlight the current choice if it is shown, so Enter keeps it.
      const current = query.trim() ? -1 : top.findIndex((m) => m.id === input.value.trim());
      if (top.length) setActive(current >= 0 ? current : 0);
    }
    pop.hidden = false;
    input.setAttribute('aria-expanded', 'true');
  };

  const show = () => {
    open = true;
    render();
  };

  const hide = () => {
    if (!open) return;
    open = false;
    pop.hidden = true;
    input.setAttribute('aria-expanded', 'false');
    input.removeAttribute('aria-activedescendant');
  };

  const pick = (index) => {
    const m = items[index];
    if (!m) return;
    input.value = m.id;
    query = '';
    hide();
    onPick(m);
  };

  input.addEventListener('focus', () => {
    query = '';
    input.select();
    show();
  });
  input.addEventListener('click', () => {
    if (!open) show();
  });
  input.addEventListener('input', () => {
    query = input.value;
    show();
  });
  input.addEventListener('blur', hide);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (!open) return show();
      setActive(active + (e.key === 'ArrowDown' ? 1 : -1));
    } else if (e.key === 'Enter' && open && active >= 0) {
      e.preventDefault();
      pick(active);
    } else if (e.key === 'Escape' && open) {
      // Close the list, not the Settings dialog.
      e.preventDefault();
      e.stopPropagation();
      hide();
    } else if (e.key === 'Tab') {
      hide();
    }
  });

  return {
    /** Re-renders an open list, e.g. once the models have loaded. */
    refresh() {
      if (open) render();
    },
  };
}
