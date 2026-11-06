// The Map page (docs/ui-design.md §7.6): each Learn session's approved plan
// drawn as a layered map of ideas — what an idea needs sits above it — with
// the learner's check results in glyphs and words. Every box is a native
// checkbox label, so the drawing is also the accessible list; the selection
// feeds the desk's "Practise" button. Rendered once per visit; nothing moves.

import { BOX, layoutPlan } from './map-layout.js';

const INTRO = 'Ideas from the plans of your Learn sessions. What an idea needs sits above it. Pick ideas to practise them together.';
const EMPTY = 'Nothing to map yet. Ideas appear here once you approve a plan in a Learn session.';
const DETAIL_HINT = 'Focus or pick an idea to see what it needs.';

const STATES = {
  checked: { word: 'checked', glyph: '✓' },
  missed: { word: 'needs work', glyph: '◐' },
  unchecked: { word: 'not checked yet', glyph: '' },
};
const stateOf = (node) => (node.correct ? 'checked' : node.missed ? 'missed' : 'unchecked');

const SVG = 'http://www.w3.org/2000/svg';
const dayMonth = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short' });

let uid = 0;

function h(tag, className, text) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text !== undefined) el.textContent = text;
  return el;
}

function svg(tag, attrs) {
  const el = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  return el;
}

function countText(nodes) {
  const n = { checked: 0, missed: 0, unchecked: 0 };
  for (const node of nodes) n[stateOf(node)]++;
  const parts = [`${nodes.length} ${nodes.length === 1 ? 'idea' : 'ideas'}`];
  if (n.checked) parts.push(`${n.checked} checked`);
  if (n.missed) parts.push(`${n.missed} ${n.missed === 1 ? 'needs' : 'need'} work`);
  return parts.join(', ');
}

/** Prerequisite edges, from the bottom of an idea to the top of what needs it. */
function edgesSvg(layout) {
  const marker = `map-arrow-${++uid}`;
  const root = svg('svg', {
    class: 'map-edges',
    width: layout.width,
    height: layout.height,
    viewBox: `0 0 ${layout.width} ${layout.height}`,
    'aria-hidden': 'true',
    focusable: 'false',
  });
  const defs = svg('defs', {});
  const m = svg('marker', { id: marker, viewBox: '0 0 8 8', refX: '8', refY: '4', markerWidth: '7', markerHeight: '7', orient: 'auto' });
  m.append(svg('path', { d: 'M0 0 L8 4 L0 8 z', fill: 'currentColor' }));
  defs.append(m);
  root.append(defs);
  for (const { from, to } of layout.edges) {
    const a = layout.pos.get(from);
    const b = layout.pos.get(to);
    const x1 = a.x + BOX.w / 2;
    const y1 = a.y + BOX.h;
    const x2 = b.x + BOX.w / 2;
    const y2 = b.y - 1;
    const dy = Math.max(16, (y2 - y1) / 2);
    root.append(svg('path', { d: `M${x1} ${y1} C${x1} ${y1 + dy} ${x2} ${y2 - dy} ${x2} ${y2}`, 'marker-end': `url(#${marker})` }));
  }
  return root;
}

/**
 * @param {HTMLElement} root  the page column
 * @param {{ onChange(count: number): void }} hooks
 */
export function createMap(root, { onChange }) {
  const selected = new Set(); // keys "sessionId\0nodeId"; survives re-renders
  let entries = new Map(); // key -> { input, node, plan, group }, in display order
  let groups = [];

  const changed = () => onChange(selected.size);

  function syncGroup(group) {
    let on = 0;
    for (const key of group.keys) if (selected.has(key)) on++;
    group.all.checked = on > 0 && on === group.keys.length;
    group.all.indeterminate = on > 0 && on < group.keys.length;
  }

  function set(key, on) {
    const entry = entries.get(key);
    if (!entry) return;
    entry.input.checked = on;
    if (on) selected.add(key);
    else selected.delete(key);
  }

  function detailText(entry) {
    const { node, plan } = entry;
    const title = (id) => plan.nodes.find((n) => n.id === id)?.title;
    const needs = node.requires.map(title).filter(Boolean);
    const leads = plan.nodes.filter((n) => n.requires.includes(node.id)).map((n) => n.title);
    const parts = [node.title, node.kind, STATES[stateOf(node)].word];
    if (node.section) parts.push(`Section: ${node.section}`);
    if (needs.length) parts.push(`Needs: ${needs.join(', ')}`);
    if (leads.length) parts.push(`Leads to: ${leads.join(', ')}`);
    return parts.join(' · ');
  }

  function groupEl(plan, open) {
    const layout = layoutPlan(plan.nodes);
    const byId = new Map(plan.nodes.map((n) => [n.id, n]));
    const details = h('details', 'map-group');
    details.open = open;
    details.append(h('summary', 'map-goal', plan.goal || plan.title));
    const when = Date.parse(plan.updatedAt);
    const date = Number.isNaN(when) ? '' : ` · ${dayMonth.format(when)}`;
    details.append(h('p', 'map-meta', `From “${plan.title}”${date} · ${countText(plan.nodes)}`));

    const allLabel = h('label', 'map-all');
    const all = h('input');
    all.type = 'checkbox';
    allLabel.append(all, ` Select all ${plan.nodes.length}`);
    details.append(allLabel);

    const scroll = h('div', 'map-scroll');
    const graph = h('div', 'map-graph');
    graph.setAttribute('role', 'group');
    graph.setAttribute('aria-label', `Ideas for ${plan.goal || plan.title}`);
    graph.style.inlineSize = `${layout.width}px`;
    graph.style.blockSize = `${layout.height}px`;
    graph.append(edgesSvg(layout));

    const group = { details, all, keys: [] };
    const descs = h('div');
    descs.hidden = true;
    for (const id of layout.order) {
      const node = byId.get(id);
      const key = `${plan.sessionId}\u0000${id}`;
      const state = stateOf(node);
      const { x, y } = layout.pos.get(id);
      const label = h('label', `map-node is-${state}`);
      label.style.left = `${x}px`;
      label.style.top = `${y}px`;
      const input = h('input');
      input.type = 'checkbox';
      input.dataset.key = key;
      const descId = `map-desc-${++uid}`;
      input.setAttribute('aria-describedby', descId);
      const text = h('span', 'map-node-text');
      text.append(h('span', 'map-node-title', node.title), h('span', 'sr-only', `, ${STATES[state].word}`));
      label.append(input, text);
      const glyph = STATES[state].glyph;
      if (glyph) {
        const g = h('span', 'map-glyph', glyph);
        g.setAttribute('aria-hidden', 'true');
        label.append(g);
      }
      graph.append(label);
      const entry = { input, node, plan, group };
      entries.set(key, entry);
      group.keys.push(key);
      const needs = node.requires.map((r) => byId.get(r)?.title).filter(Boolean);
      descs.append(Object.assign(h('span', '', needs.length ? `Needs: ${needs.join(', ')}.` : 'Needs nothing else in this plan.'), { id: descId }));
    }
    scroll.append(graph);
    const detail = h('p', 'map-detail', DETAIL_HINT);
    details.append(scroll, detail, descs);

    details.addEventListener('change', (e) => {
      const t = e.target;
      if (t === all) {
        for (const key of group.keys) set(key, all.checked);
      } else if (t.dataset.key) {
        set(t.dataset.key, t.checked);
        detail.textContent = detailText(entries.get(t.dataset.key));
      } else return;
      syncGroup(group);
      changed();
    });
    details.addEventListener('focusin', (e) => {
      const key = e.target.dataset?.key;
      if (key) detail.textContent = detailText(entries.get(key));
    });
    // A drawing wider than its box starts centred (the widest layer is
    // centred too); read once per group, when it first shows.
    let centred = false;
    group.centre = () => {
      if (centred || !details.open) return;
      centred = true;
      const spare = scroll.scrollWidth - scroll.clientWidth;
      if (spare > 0) scroll.scrollLeft = spare / 2;
    };
    details.addEventListener('toggle', group.centre);
    groups.push(group);
    return details;
  }

  function agendaEl() {
    const missed = [];
    const checked = [];
    for (const [key, { node }] of entries) {
      const s = stateOf(node);
      if (s === 'missed') missed.push(key);
      else if (s === 'checked') checked.push(key);
    }
    if (!missed.length && !checked.length) return null;
    const p = h('p', 'map-agenda', 'Select: ');
    const pick = (label, keys) => {
      const b = h('button', 'btn text', `${label} (${keys.length})`);
      b.type = 'button';
      b.addEventListener('click', () => {
        for (const key of keys) {
          set(key, true);
          entries.get(key).group.details.open = true;
        }
        groups.forEach(syncGroup);
        changed();
      });
      return b;
    };
    const buttons = [];
    if (missed.length) buttons.push(pick('Needs work', missed));
    if (checked.length) buttons.push(pick('Checked', checked));
    buttons.forEach((b, i) => {
      if (i) {
        const sep = h('span', '', ' · ');
        sep.setAttribute('aria-hidden', 'true');
        p.append(sep);
      }
      p.append(b);
    });
    return p;
  }

  /** Renders the map; the selection keeps the ideas that still exist. */
  function render(plans) {
    entries = new Map();
    groups = [];
    if (!plans.length) {
      root.replaceChildren(h('p', 'map-status', EMPTY));
      selected.clear();
      changed();
      return;
    }
    const list = plans.map((plan, i) => groupEl(plan, i === 0));
    for (const key of [...selected]) if (!entries.has(key)) selected.delete(key);
    for (const key of selected) entries.get(key).input.checked = true;
    groups.forEach(syncGroup);
    const intro = h('p', 'map-intro', INTRO);
    const agenda = agendaEl();
    root.replaceChildren(intro, ...(agenda ? [agenda] : []), ...list);
    for (const group of groups) group.centre();
    changed();
  }

  function loading() {
    root.replaceChildren(h('p', 'map-status', 'Loading the map…'));
  }

  function failed(text, retry) {
    const p = h('p', 'map-status danger', `Couldn’t load the map: ${text} `);
    p.setAttribute('role', 'status');
    const b = h('button', 'btn text', 'Try again');
    b.type = 'button';
    b.addEventListener('click', retry);
    p.append(b);
    root.replaceChildren(p);
  }

  function clear() {
    for (const key of [...selected]) set(key, false);
    selected.clear();
    groups.forEach(syncGroup);
    changed();
  }

  /** The selected ideas in display order (plan, then layer). */
  function selection() {
    const out = [];
    for (const [key, { node, plan }] of entries) if (selected.has(key)) out.push({ title: node.title, goal: plan.goal || plan.title });
    return out;
  }

  return { render, loading, failed, clear, selection };
}
