// The Map page (docs/ui-design.md §7.6): the learner's knowledge graph as a
// quiet constellation. One cluster per topic; every prerequisite edge in the
// store runs between the ideas it links, across topics too. Ideas are styled
// by what they are (mechanism, fact, procedure, perceptual, argument), never
// by a check state the graph does not record.
//
// Nothing moves but the learner's own hand: the opening burst, dragging and
// flinging, panning, zooming, the click shockwave, the shockwave ring around a
// pick. The edges are still lines — the flowing light and the breathing halos
// were removed on purpose (§8).
//
// Accessibility: the canvas is decoration over a visually hidden list of
// native checkboxes (one per idea, grouped per topic). Tab walks that list;
// the canvas draws the focus ring, the detail card and pans to the idea. Each
// idea's "Depends on" / "Leads to" labels follow its checkbox, so the flyout
// is reachable from the keyboard as well as by hovering the card.
//
// An edge has a relation. `requires` is the only one drawn: it is the line, the
// "Depends on"/"Leads to" flyout, the layering and the arrowhead. A `contrasts`
// edge orders nothing (the two claims are easily confused, not sequenced), so
// it never becomes a link — it is a "Contrasts with …" line on the two cards.

import { call, message } from './api.js';
import { calibrationSummary, venueGroups } from './fieldguide.js';
import { active, createWorld, hit, reheat, settle, tick } from './map-physics.js';

const EMPTY = 'Nothing in the knowledge map yet.';

// The stage is always dark: light on dark is what lets a drawing read.
// Contrast against the stage's lightest point (bg1): TEXT 13.9:1, dim 6.1:1.
const C = {
  bg0: '#0a0f0d',
  bg1: '#15221d',
  text: '#F0ECE4',
  textDim: 'rgba(240, 236, 228, 0.62)',
  edge: 'rgba(233, 229, 221, 0.17)',
  edgeHot: 'rgba(233, 229, 221, 0.42)',
  idle: 'rgba(233, 229, 221, 0.45)',
  idleFill: 'rgba(233, 229, 221, 0.06)',
  select: '#F6EBD6',
  focus: '#A6D4C1',
  // A folded topic: a quiet mint disc, its count and name in the label ink.
  topicFill: 'rgba(166, 212, 193, 0.10)',
  topicRing: 'rgba(166, 212, 193, 0.55)',
  // A link between folded topics carries several prerequisites: a touch brighter.
  link: 'rgba(233, 229, 221, 0.26)',
  linkHot: 'rgba(233, 229, 221, 0.5)',
};

// One ink per kind of proposition. Colour *and* ring weight (and, for the two
// quieter kinds, a dashed ring), so the difference survives a monochrome
// display and forced-colors mode [Haroz & Whitney 2012].
const KIND = {
  mechanism: { ink: '#8CC4AE', tint: 'rgba(140, 196, 174, 0.16)', weight: 600, ring: 2.2, dash: [] },
  fact: { ink: '#E8B86A', tint: 'rgba(232, 184, 106, 0.16)', weight: 500, ring: 1.8, dash: [] },
  procedure: { ink: '#9FC4D9', tint: 'rgba(159, 196, 217, 0.16)', weight: 600, ring: 2.2, dash: [] },
  perceptual: { ink: '#C6B0D9', tint: 'rgba(198, 176, 217, 0.16)', weight: 500, ring: 1.8, dash: [5, 3] },
  argument: { ink: '#D9A99A', tint: 'rgba(217, 169, 154, 0.16)', weight: 500, ring: 1.8, dash: [2, 3] },
};
const UNKNOWN_KIND = { ink: C.idle, tint: C.idleFill, weight: 500, ring: 1.5, dash: [3, 3] };
const kindOf = (node) => KIND[node.kind] ?? UNKNOWN_KIND;
const kindName = (node) => (KIND[node.kind] ? node.kind : 'idea');

const FONT_UI = 'system-ui, -apple-system, "Segoe UI", Roboto, "Noto Sans", sans-serif';
const FONT_READ = '"Iowan Old Style", Charter, "Noto Serif", Georgia, serif';
/** Zoom floor for a small map; a big one may go further out (see limits()). */
const K_MIN = 0.3;
/** Never further out than this, whatever the map's size. */
const K_FLOOR = 0.02;
const K_MAX = 2.4;
const HIDE_MS = 150;
/**
 * Semantic zoom (ui-design §7.6): a topic opens when its block is at least
 * OPEN_PX across on screen and folds back into one disc under CLOSE_PX (the
 * gap keeps a zoom resting on the threshold from flickering). At K_OPEN_ALL
 * and closer — where idea labels start to show — every topic is open.
 */
const OPEN_PX = 220;
const CLOSE_PX = 180;
const K_OPEN_ALL = 0.5;
/** Share of the remaining fade covered per frame: ~150 ms at 60 Hz. */
const FADE = 0.22;

let uid = 0;

function h(tag, className, text) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text !== undefined) el.textContent = text;
  return el;
}

const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
const easeOut = (p) => 1 - (1 - p) ** 3;

// An edge's `relation` says what it claims. Only `requires` orders the two
// ideas — it is what the map draws as an arrow, what the layout layers by and
// what "Depends on" counts. `contrasts` says the claims are easily confused:
// plain text on the two cards, never a link. A line written before the field
// existed has no relation and means `requires`.
const isRequires = (edge) => (edge.relation ?? 'requires') === 'requires';
const isContrast = (edge) => edge.relation === 'contrasts';

const withAlpha = (hex, a) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
};

/** A soft round light, drawn once per colour and stamped with 'lighter'. */
function sprite(color) {
  const c = document.createElement('canvas');
  c.width = c.height = 64;
  const g = c.getContext('2d');
  const grad = g.createRadialGradient(32, 32, 0, 32, 32, 32);
  grad.addColorStop(0, color);
  grad.addColorStop(0.25, withAlpha(color, 0.45));
  grad.addColorStop(1, withAlpha(color, 0));
  g.fillStyle = grad;
  g.fillRect(0, 0, 64, 64);
  return c;
}

/** Splits a title into at most two lines of at most `max` px. */
function wrap(ctx, text, max) {
  const words = text.split(/\s+/);
  const lines = [''];
  for (const w of words) {
    const line = lines[lines.length - 1];
    const next = line ? `${line} ${w}` : w;
    if (ctx.measureText(next).width <= max || !line) lines[lines.length - 1] = next;
    else if (lines.length < 2) lines.push(w);
    else {
      lines[1] = `${lines[1]} ${w}`;
      break;
    }
  }
  if (lines.length === 2 && ctx.measureText(lines[1]).width > max) {
    let s = lines[1];
    while (s.length > 1 && ctx.measureText(`${s}…`).width > max) s = s.slice(0, -1);
    lines[1] = `${s.trimEnd()}…`;
  }
  return lines;
}

/** Every line a text needs at `max` px — a topic path is never truncated. */
function split(ctx, text, max) {
  const lines = [];
  let line = '';
  for (const word of text.split(/\s+/)) {
    const next = line ? `${line} ${word}` : word;
    if (line && ctx.measureText(next).width > max) {
      lines.push(line);
      line = word;
    } else line = next;
  }
  if (line) lines.push(line);
  return lines.length ? lines : [''];
}

/** Point and tangent on the edge curve a → b at u ∈ [0, 1]. */
function curve(e, u) {
  const { a, b } = e;
  const x0 = a.x;
  // Leave from under the label, so no edge crosses its own idea's name.
  const y0 = a.y + a.r + 10 + 16 * (a.lines?.length ?? 1);
  const x3 = b.x;
  const y3 = b.y - b.r - 3;
  const dy = Math.max(20, (y3 - y0) / 2);
  const y1 = y0 + dy;
  const y2 = y3 - dy;
  const m = 1 - u;
  const x = m * m * m * x0 + 3 * m * m * u * x0 + 3 * m * u * u * x3 + u * u * u * x3;
  const y = m * m * m * y0 + 3 * m * m * u * y1 + 3 * m * u * u * y2 + u * u * u * y3;
  const tx = 6 * m * u * (x3 - x0);
  const ty = 3 * m * m * (y1 - y0) + 6 * m * u * (y2 - y1) + 3 * u * u * (y3 - y2);
  return { x, y, tx, ty, x0, y0, x3, y3, y1, y2 };
}

/**
 * @param {HTMLElement} root  the map stage
 * @param {{ onChange(count: number): void }} hooks
 */
export function createMap(root, { onChange, onReviewDecided = null, onStartFrontier = null, onOpenSession = null }) {
  const selected = new Set(); // node ids; survives re-renders
  let entries = new Map(); // node id -> { input, node }
  let learn = null; // the newest plan's learning context (levels, calibration, frontier)
  /** Topic id → its own + descendants' venues, from `knowledge_map.topicVenues`. */
  let venuesByTopic = new Map();

  const canvas = h('canvas', 'map-canvas');
  canvas.setAttribute('aria-hidden', 'true');
  const hud = h('div', 'map-hud');
  const stats = h('p', 'map-stats');
  // Where the field lives (ui-design §7.6, §7.7.6): a venue belongs to a topic
  // in the graph, so the guide is a list of the topics that have one, and a row
  // jumps to that topic. A collapsible panel in the page's own chrome, not a
  // modal: nothing here covers the map. Hidden while no topic has a venue.
  const guide = h('details', 'map-guide');
  guide.hidden = true;
  // The confidence line and the frontier list for the newest session
  // (ui-design §7.7.6). Hidden until a session brings some.
  const learning = h('div', 'map-learning');
  learning.hidden = true;
  hud.append(stats, guide, learning);
  const fitButton = h('button', 'btn text map-fit', 'Fit');
  fitButton.type = 'button';
  fitButton.setAttribute('aria-label', 'Frame the map at a readable zoom');
  const a11y = h('div', 'map-a11y');
  const card = h('div', 'map-card');
  card.hidden = true;
  const flyout = h('div', 'map-flyout');
  flyout.hidden = true;
  const status = h('div', 'map-status');
  // The proposed-links review sits beside Fit; its dialog is page-level (a
  // modal over the whole window, not clipped by the stage).
  const proposalsButton = h('button', 'btn text map-proposals', 'Review proposed links');
  proposalsButton.type = 'button';
  proposalsButton.hidden = true;
  const proposalsDialog = h('dialog', 'proposals');
  proposalsDialog.setAttribute('aria-labelledby', 'proposals-heading');
  document.body.append(proposalsDialog);
  // Topic splits the splitter proposes (arch §4.5): reviewed like links, in a
  // page-level modal; nothing is restructured until the learner accepts.
  const splitsButton = h('button', 'btn text map-splits', 'Review topic splits');
  splitsButton.type = 'button';
  splitsButton.hidden = true;
  const splitsDialog = h('dialog', 'proposals splits');
  splitsDialog.setAttribute('aria-labelledby', 'splits-heading');
  document.body.append(splitsDialog);
  // Moving an idea to another topic without dragging (WCAG 2.5.7), and the
  // confirmation a drag ends in.
  const moveDialog = h('dialog', 'proposals map-move');
  moveDialog.setAttribute('aria-labelledby', 'map-move-heading');
  document.body.append(moveDialog);
  const review2 = h('div', 'map-reviews');
  review2.append(proposalsButton, splitsButton);
  // Zoom by hand without a wheel or a pinch, and the way back to the newest
  // session's ideas (shown only when some of them are on the map).
  const zoomBox = h('div', 'map-zoom');
  const currentButton = h('button', 'btn text map-current', 'Last session');
  currentButton.type = 'button';
  currentButton.setAttribute('aria-label', "Zoom to your newest session's ideas");
  currentButton.hidden = true;
  const zoomOutButton = h('button', 'btn text map-zoom-btn', '−');
  zoomOutButton.type = 'button';
  zoomOutButton.setAttribute('aria-label', 'Zoom out');
  const zoomInButton = h('button', 'btn text map-zoom-btn', '+');
  zoomInButton.type = 'button';
  zoomInButton.setAttribute('aria-label', 'Zoom in');
  zoomBox.append(currentButton, zoomOutButton, zoomInButton);
  zoomBox.hidden = true;
  root.replaceChildren(canvas, hud, review2, fitButton, zoomBox, a11y, card, flyout, status);
  const ctx = canvas.getContext('2d');

  const glow = { white: sprite('#F6EBD6') };
  for (const [name, kind] of Object.entries(KIND)) glow[name] = sprite(kind.ink);

  let world = null;
  let W = 0;
  let H = 0;
  let dpr = 1;
  let bg = null;
  const cam = { x: 0, y: 0, k: 1 };
  let camTarget = null;
  let t0 = 0;
  let raf = 0;
  let lastT = 0;
  let hover = null;
  let focusNode = null;
  let focusCluster = null;
  let drag = null;
  const pointers = new Map();
  let pinch = null;
  let sparks = [];
  let rings = [];
  let stars = [];
  let reduced = false;
  let forced = false;
  let counts = { ideas: 0, topics: 0, links: 0 };
  /** Zoom floor for the current map and stage (limits()). */
  let kMin = K_MIN;
  /** True while a topic is folding or unfolding. */
  let fading = false;
  /** Folded topics drawn this frame, in stage px: `{ t, x, y, r, a }`. */
  let bubbles = [];
  /** The folded topic under the pointer. */
  let hoverTopic = null;
  let statEls = {};
  let hat = 0; // hide timer
  let cardFor = null;
  let flyoutFor = null;
  let review = { proposals: [], titles: {} };
  let decisionBusy = false;
  /** Idea id → titles of the claims it is easily confused with. */
  let contrastTitles = new Map();
  /** The splitter's proposals still awaiting a decision (`graph_splits`). */
  let splitReview = [];
  let splitBusy = false;
  /** Topic id → its full path, for every topic in the graph. */
  let topicPaths = new Map();
  /** Idea id → its `node_provenance` record (a promise), fetched on first show. */
  let origins = new Map();

  // ---- proposed links (the extract_edges review surface) ------------------
  // The extractor proposes edges with a reason each — a prerequisite
  // (`requires`) or a confusion (`contrasts`). The graph only takes one when
  // the learner accepts it here, and the core re-validates on accept, so this
  // list can be wrong without being dangerous.

  function titleOf(id) {
    return review.titles?.[id] ?? id;
  }

  /** A proposal's address: the backend keys a decision by (from, to, relation). */
  function proposalKey(p) {
    return `${p.from}\u0000${p.to}\u0000${p.relation ?? 'requires'}`;
  }

  /** "2 prerequisites, 1 contrast" from the decision's per-relation counts. */
  function relationPhrase(counts) {
    const words = { requires: ['prerequisite', 'prerequisites'], contrasts: ['contrast', 'contrasts'] };
    const named = Object.keys(counts).filter((k) => counts[k]);
    const order = (k) => (k === 'requires' ? 0 : 1);
    return named
      .sort((a, b) => order(a) - order(b) || a.localeCompare(b))
      .map((k) => {
        const n = counts[k];
        const [one, many] = words[k] ?? [k, `${k}s`];
        return `${n} ${n === 1 ? one : many}`;
      })
      .join(', ');
  }

  function drawProposals() {
    const list = review.proposals ?? [];
    proposalsButton.hidden = list.length === 0;
    proposalsButton.textContent = `Review ${list.length} proposed link${list.length === 1 ? '' : 's'}`;
  }

  function proposalRow(p, onCollect) {
    const key = proposalKey(p);
    const li = h('li', 'proposal');
    li.append(h('p', 'proposal-edge', `${titleOf(p.from)} → ${titleOf(p.to)}`));
    li.append(h('p', 'proposal-relation', p.relation === 'contrasts' ? 'Easily confused with' : 'Prerequisite of'));
    if (p.why) li.append(h('p', 'proposal-why', p.why));
    const actions = h('div', 'proposal-actions');
    const yes = h('button', 'btn text', 'Accept');
    yes.type = 'button';
    yes.addEventListener('click', () => {
      onCollect.accept.push(key);
      li.classList.add('is-accepted');
      actions.replaceChildren(h('span', 'proposal-state', 'Accepted — saved when you close this list.'));
    });
    const no = h('button', 'btn text', 'Dismiss');
    no.type = 'button';
    no.addEventListener('click', () => {
      onCollect.dismiss.push(key);
      li.remove();
    });
    actions.append(yes, no);
    li.append(actions);
    return li;
  }

  function openProposals() {
    const list = review.proposals ?? [];
    const collect = { accept: [], dismiss: [] };
    const head = h('div', 'proposals-head');
    const title = h('h2', '', 'Proposed links');
    title.id = 'proposals-heading';
    title.tabIndex = -1;
    const close = h('button', 'btn text', 'Close');
    close.type = 'button';
    head.append(title, close);
    const body = h('div', 'proposals-body');
    body.append(
      h(
        'p',
        'proposals-intro',
        'Each row is an edge the extractor proposed, with its reason. Accept adds it to the map; nothing here is added until you accept it.',
      ),
    );
    const ul = h('ol', 'proposals-list');
    for (const p of list) ul.append(proposalRow(p, collect));
    body.append(ul);
    const foot = h('div', 'proposals-foot');
    const all = h('button', 'btn text', 'Accept all');
    all.type = 'button';
    all.addEventListener('click', () => {
      for (const p of list) {
        const key = proposalKey(p);
        if (!collect.accept.includes(key) && !collect.dismiss.includes(key)) collect.accept.push(key);
      }
      for (const li of ul.querySelectorAll('.proposal')) li.classList.add('is-accepted');
      all.disabled = true;
    });
    const saveStatus = h('p', 'proposals-status');
    foot.append(all, saveStatus);
    body.append(foot);

    close.addEventListener('click', () => proposalsDialog.close());
    proposalsDialog.onclose = async () => {
      if (!collect.accept.length && !collect.dismiss.length) return;
      if (decisionBusy) return;
      decisionBusy = true;
      try {
        const out = await call('decide_edge_proposals', {
          accept: collect.accept,
          dismiss: collect.dismiss,
        });
        const added = out?.accepted?.length ?? 0;
        const counted = relationPhrase(out?.acceptedByRelation ?? {});
        const dismissed = relationPhrase(out?.dismissedByRelation ?? {});
        const refused = out?.rejected ?? [];
        const said = [];
        if (added) said.push(`${counted || `${added} link${added === 1 ? '' : 's'}`} added to the map`);
        if (dismissed) said.push(`${dismissed} dismissed`);
        const text = `${said.length ? said.join('; ') : 'No links added'}${
          refused.length ? `; ${refused.length} refused: ${refused.map((r) => r.reason).join(', ')}` : ''
        }.`;
        proposalsDialog.replaceChildren();
        // Refresh first — the Map clears its own status line when it re-renders
        // — then say what happened, where the learner can still see it.
        await onReviewDecided?.();
        status.replaceChildren(h('p', refused.length ? 'map-status-text danger' : 'map-status-text', text));
      } catch (err) {
        status.replaceChildren(h('p', 'map-status-text danger', `Couldn't save the links: ${message(err)}`));
      } finally {
        decisionBusy = false;
      }
    };
    body.prepend(head);
    proposalsDialog.replaceChildren(body);
    proposalsDialog.showModal();
    title.focus({ preventScroll: true });
  }

  proposalsDialog.addEventListener('click', (e) => {
    if (e.target === proposalsDialog) proposalsDialog.close();
  });
  proposalsButton.addEventListener('click', openProposals);

  // ---- topic splits (the splitter's review surface, arch §4.5) ------------
  // A topic grown past 30 ideas in groups that barely depend on each other is
  // proposed as sub-topics. The groups' names are suggestions: the learner can
  // rename each part before accepting, and nothing moves until they accept.

  const pathOfTopic = (id) => topicPaths.get(id) ?? id;
  const ideaTitle = (id) => entries.get(id)?.node.title ?? id;
  const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

  function drawSplits() {
    splitsButton.hidden = splitReview.length === 0;
    splitsButton.textContent = `Review ${plural(splitReview.length, 'topic split')}`;
  }

  function splitRow(p, collect) {
    const li = h('li', 'proposal split');
    const ideas = p.groups.reduce((n, g) => n + (g.nodeIds?.length ?? 0), 0);
    li.append(h('p', 'proposal-edge', pathOfTopic(p.topic)));
    li.append(h('p', 'proposal-relation', `${plural(ideas, 'idea')} → ${plural(p.groups.length, 'new topic')} inside it`));
    const groups = h('ol', 'split-groups');
    const names = p.groups.map((g, i) => {
      const row = h('li', 'split-group');
      const id = `split-name-${++uid}`;
      const label = h('label', 'split-name-label', `Name for part ${i + 1}`);
      label.htmlFor = id;
      const input = h('input', 'input split-name');
      input.type = 'text';
      input.id = id;
      input.value = g.title ?? '';
      row.append(label, input);
      if (g.why) row.append(h('p', 'proposal-why', g.why));
      const list = h('details', 'split-ideas');
      list.append(h('summary', '', plural(g.nodeIds?.length ?? 0, 'idea')));
      const ul = h('ul');
      for (const nid of g.nodeIds ?? []) ul.append(h('li', '', ideaTitle(nid)));
      list.append(ul);
      row.append(list);
      groups.append(row);
      return input;
    });
    li.append(groups);
    const actions = h('div', 'proposal-actions');
    const yes = h('button', 'btn text', 'Accept');
    yes.type = 'button';
    yes.addEventListener('click', () => {
      // The names are read when the list closes, so a rename after Accept counts.
      collect.accept.set(p.topic, () => names.map((n) => n.value.trim()));
      li.classList.add('is-accepted');
      actions.replaceChildren(h('span', 'proposal-state', 'Accepted — saved when you close this list.'));
    });
    const no = h('button', 'btn text', 'Dismiss');
    no.type = 'button';
    no.addEventListener('click', () => {
      collect.dismiss.push(p.topic);
      li.remove();
    });
    actions.append(yes, no);
    li.append(actions);
    return li;
  }

  function openSplits() {
    const collect = { accept: new Map(), dismiss: [] };
    const head = h('div', 'proposals-head');
    const title = h('h2', '', 'Topic splits');
    title.id = 'splits-heading';
    title.tabIndex = -1;
    const close = h('button', 'btn text', 'Close');
    close.type = 'button';
    close.addEventListener('click', () => splitsDialog.close());
    head.append(title, close);
    const body = h('div', 'proposals-body');
    body.append(
      head,
      h(
        'p',
        'proposals-intro',
        'These topics have grown into groups of ideas that barely depend on each other. Accept makes each group a topic inside the old one, under the names below; nothing changes until you accept.',
      ),
    );
    const ol = h('ol', 'proposals-list');
    for (const p of splitReview) ol.append(splitRow(p, collect));
    body.append(ol);
    splitsDialog.onclose = async () => {
      if ((!collect.accept.size && !collect.dismiss.length) || splitBusy) return;
      splitBusy = true;
      try {
        const titles = Object.fromEntries([...collect.accept].map(([topic, read]) => [topic, read()]));
        const out = await call('decide_splits', { accept: [...collect.accept.keys()], dismiss: collect.dismiss, titles });
        const said = [];
        const split = out?.accepted ?? [];
        if (split.length) said.push(`Split ${split.map((t) => `“${pathOfTopic(t)}”`).join(', ')} into new topics`);
        if (out?.dismissed?.length) said.push(`${plural(out.dismissed.length, 'split')} dismissed`);
        splitsDialog.replaceChildren();
        await onReviewDecided?.();
        status.replaceChildren(h('p', 'map-status-text', `${said.join('; ')}.`));
      } catch (err) {
        status.replaceChildren(h('p', 'map-status-text danger', `Couldn't save the splits: ${message(err)}`));
      } finally {
        splitBusy = false;
      }
    };
    splitsDialog.replaceChildren(body);
    splitsDialog.showModal();
    title.focus({ preventScroll: true });
  }
  splitsDialog.addEventListener('click', (e) => {
    if (e.target === splitsDialog) splitsDialog.close();
  });
  splitsButton.addEventListener('click', openSplits);

  // ---- moving an idea to another topic (arch §4.5, a learner `place`) -----
  // Dropping a dragged idea on another topic asks here first, with that topic
  // chosen; the card's "Move to another topic…" opens the same dialog, so the
  // move never needs a drag (WCAG 2.5.7).

  function openMove(node, targetId = null) {
    const opener = document.activeElement;
    const head = h('div', 'proposals-head');
    const title = h('h2', '', 'Move an idea');
    title.id = 'map-move-heading';
    title.tabIndex = -1;
    head.append(title);
    const form = h('form', 'map-move-form');
    form.noValidate = true;
    form.append(head, h('p', 'proposals-intro', `“${node.title}” is in ${node.cluster.path}.`));
    const choices = [...topicPaths].filter(([id]) => id !== node.cluster.id).sort((a, b) => a[1].localeCompare(b[1]));
    const stateLine = h('p', 'proposals-status');
    stateLine.setAttribute('role', 'status');
    const actions = h('div', 'proposals-foot');
    const cancel = h('button', 'btn text', 'Cancel');
    cancel.type = 'button';
    cancel.addEventListener('click', () => moveDialog.close());
    let select = null;
    if (choices.length) {
      const field = h('div', 'field');
      const id = `map-move-to-${++uid}`;
      const label = h('label', '', 'Move it to');
      label.htmlFor = id;
      select = h('select', 'input map-move-select');
      select.id = id;
      for (const [tid, path] of choices) {
        const opt = h('option', '', path);
        opt.value = tid;
        opt.selected = tid === targetId;
        select.append(opt);
      }
      field.append(label, select);
      form.append(field, h('p', 'help', 'Your move stays: the graph builder never moves it back.'));
      const go = h('button', 'btn primary', 'Move');
      go.type = 'submit';
      actions.append(go, cancel);
    } else {
      form.append(h('p', 'help', 'There is no other topic to move it to.'));
      actions.append(cancel);
    }
    form.append(actions, stateLine);
    let moved = false;
    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      if (!select) return;
      const home = select.value;
      for (const c of form.querySelectorAll('button, select')) c.disabled = true;
      stateLine.textContent = 'Moving…';
      try {
        await call('place_node', { nodeId: node.id, home });
        moved = true;
        moveDialog.close();
        await onReviewDecided?.();
        status.replaceChildren(h('p', 'map-status-text', `Moved “${node.title}” to ${pathOfTopic(home)}.`));
      } catch (err) {
        for (const c of form.querySelectorAll('button, select')) c.disabled = false;
        stateLine.textContent = `Couldn't move it: ${message(err)}`;
        stateLine.classList.add('danger');
      }
    });
    moveDialog.onclose = () => {
      moveDialog.replaceChildren();
      if (!moved && opener?.isConnected) opener.focus({ preventScroll: true });
    };
    moveDialog.replaceChildren(form);
    moveDialog.showModal();
    (targetId && select ? form.querySelector('button[type="submit"]') : select ?? cancel).focus({ preventScroll: true });
  }
  moveDialog.addEventListener('click', (e) => {
    if (e.target === moveDialog) moveDialog.close();
  });

  /** The other topic a dragged idea is over (an open one with ideas), or null. */
  function dropTarget(w, n) {
    let best = null;
    for (const c of world?.clusters ?? []) {
      if (c === n.cluster || (c.topic?.shown ?? 1) < 0.5) continue;
      if (w.x < c.x0 - 24 || w.x > c.x1 + 24 || w.y < c.y0 - 24 || w.y > c.y1 + 24) continue;
      if (!best || (c.topic?.depth ?? 0) > (best.topic?.depth ?? 0)) best = c;
    }
    return best;
  }

  // ---- where an idea came from (arch §4.4, `node_provenance`) -------------

  function originOf(id) {
    if (!origins.has(id)) {
      origins.set(id, call('node_provenance', { nodeId: id }).catch((err) => ({ error: message(err) })));
    }
    return origins.get(id);
  }

  function day(at) {
    const d = new Date(at);
    return Number.isNaN(d.getTime()) ? '' : d.toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' });
  }

  /** "the session ‘Title’" as a link back to it, or plain words without one. */
  function sessionRef(e) {
    const sid = e.provenance?.sessionId;
    if (!sid) return null;
    if (!e.sessionTitle) return document.createTextNode('a session no longer on this device');
    const b = h('button', 'map-card-origin-link', `“${e.sessionTitle}”`);
    b.type = 'button';
    b.setAttribute('aria-label', `Open the session ${e.sessionTitle}`);
    b.addEventListener('click', () => onOpenSession?.(sid));
    return b;
  }

  /** One step of the record, in words. */
  function originItem(e) {
    const li = h('li', 'map-card-origin-item');
    const type = e.provenance?.type ?? '';
    const ref = sessionRef(e);
    const say = (...parts) => {
      for (const p of parts) if (p) li.append(typeof p === 'string' ? document.createTextNode(p) : p);
    };
    switch (e.kind) {
      case 'placed':
        say(e.by === 'learner' ? 'Moved by you to ' : 'Placed by the graph builder in ', pathOfTopic(e.home));
        break;
      case 'split':
        say(`Moved into “${e.group}” when you split `, pathOfTopic(e.home));
        break;
      case 'merged':
        say(`Merged with “${e.otherTitle ?? e.other}”: the same claim`);
        break;
      default: {
        const again = e.kind === 'updated';
        if (type === 'import') say(again ? 'Updated by an import' : 'Imported');
        else if (type === 'hunt') say(again ? 'Restated from a hunt' : 'Brought back from a hunt', ref ? ' in ' : '', ref);
        else if (type === 'learner') say(again ? 'Edited by you' : 'Added by you');
        else if (ref) say(again ? 'Restated in ' : 'From ', ref);
        else say(again ? 'Updated' : 'Recorded');
      }
    }
    const when = day(e.at);
    if (when) li.append(h('span', 'map-card-origin-when', ` · ${when}`));
    if (e.provenance?.quote) li.append(h('q', 'map-card-origin-quote', e.provenance.quote));
    return li;
  }

  /** The card's "Where this came from": filled in when the record arrives. */
  function originSection(n) {
    const wrap = h('div', 'map-card-origin');
    const head = h('p', 'map-card-origin-title', 'Where this came from');
    head.tabIndex = -1;
    const list = h('ul', 'map-card-origin-list');
    list.append(h('li', 'map-card-origin-wait', 'Loading…'));
    wrap.append(head, list);
    originOf(n.id).then((record) => {
      if (record?.error) {
        list.replaceChildren(h('li', 'map-card-origin-wait', `Couldn't read it: ${record.error}`));
      } else {
        const events = record?.events ?? [];
        // A claim restated by many sessions keeps its first steps and its
        // latest ones; the middle is a count.
        const shown = events.length > 6 ? [...events.slice(0, 2), null, ...events.slice(-4)] : events;
        list.replaceChildren(
          ...shown.map((e) => (e ? originItem(e) : h('li', 'map-card-origin-wait', `… ${events.length - 6} more`))),
        );
        if (!events.length) list.replaceChildren(h('li', 'map-card-origin-wait', 'Nothing recorded.'));
      }
      request();
    });
    return wrap;
  }

  const changed = () => onChange(selected.size);
  const visible = () => !root.closest('[hidden]') && document.visibilityState === 'visible';
  const now = () => (performance.now() - t0) / 1000;

  // ---------- selection ----------

  function set(key, on) {
    const entry = entries.get(key);
    if (!entry) return;
    entry.input.checked = on;
    if (on) selected.add(key);
    else selected.delete(key);
  }

  function toggleNode(node) {
    const on = !selected.has(node.key);
    set(node.key, on);
    changed();
    burst(node, on ? 'white' : KIND[node.kind] ? node.kind : 'white', on ? 22 : 10);
    if (!reduced) rings.push({ x: node.x, y: node.y, r: node.r, t: now(), color: on ? C.select : C.idle });
    request();
  }

  function toggleCluster(cluster) {
    const on = !cluster.nodes.every((n) => selected.has(n.key));
    for (const n of cluster.nodes) set(n.key, on);
    changed();
    if (on) cluster.nodes.forEach((n, i) => setTimeout(() => burst(n, 'white', 6), reduced ? 0 : i * 35));
    request();
  }

  // ---------- effects ----------

  function burst(node, color, n) {
    if (reduced) return;
    for (let i = 0; i < n; i++) {
      const a = (i / n) * Math.PI * 2 + node.seed * 6;
      const s = 1.6 + ((i * 7919) % 13) / 6;
      sparks.push({ x: node.x, y: node.y, vx: Math.cos(a) * s, vy: Math.sin(a) * s, life: 0, max: 0.7 + (i % 5) * 0.08, color });
    }
  }

  // ---------- camera ----------

  function toScreen(x, y) {
    return { x: (x - cam.x) * cam.k + W / 2, y: (y - cam.y) * cam.k + H / 2 };
  }

  function toWorld(sx, sy) {
    return { x: (sx - W / 2) / cam.k + cam.x, y: (sy - H / 2) / cam.k + cam.y };
  }

  /** Centres and scales a set of cluster or topic boxes, titles included. */
  function frameOf(boxes) {
    const x0 = Math.min(...boxes.map((c) => c.x0)) - 90; // titles can overhang
    const x1 = Math.max(...boxes.map((c) => c.x1)) + 90;
    const y0 = Math.min(...boxes.map((c) => c.y0)) - 120; // the cluster titles
    const y1 = Math.max(...boxes.map((c) => c.y1)) + 56;
    const top = 96; // the HUD
    const bottom = 80; // a folded topic's name under its disc; the zoom buttons
    const k = clamp(Math.min((W - 96) / (x1 - x0), (H - top - bottom) / (y1 - y0)), K_FLOOR, 1.15);
    return { x: (x0 + x1) / 2, y: (y0 + y1) / 2 - (top - bottom) / (2 * k), k };
  }

  /**
   * Frames the whole graph. Every topic belongs on screen: the cluster titles
   * are drawn in screen space (see drawClusterTitles), so a far-out view still
   * says which topic is where instead of cropping half the map away.
   */
  function fitTarget() {
    const roots = world?.roots ?? [];
    if (!roots.length) return { x: 0, y: 0, k: 1 };
    return frameOf(roots);
  }

  /**
   * The zoom floor: K_MIN for a map that fits at that zoom, else a little
   * past the fit — and always far enough out that every root topic has
   * folded into its disc, so zooming all the way out shows the subjects.
   */
  function limits() {
    if (!world || !W) return;
    const widest = Math.max(1, ...world.roots.map((r) => Math.max(r.w, r.h)));
    kMin = Math.max(K_FLOOR, Math.min(K_MIN, fitTarget().k * 0.6, (CLOSE_PX * 0.9) / widest));
    cam.k = clamp(cam.k, kMin, K_MAX);
  }

  function fit(instant) {
    if (!world || !world.nodes.length) return;
    const target = fitTarget();
    if (instant || reduced) Object.assign(cam, target), (camTarget = null);
    else camTarget = target;
    request();
  }

  /** Zooms by `factor` about a stage point, which stays where it is. */
  function zoomAt(factor, sx, sy) {
    if (!world) return;
    const before = toWorld(sx, sy);
    cam.k = clamp(cam.k * factor, kMin, K_MAX);
    const after = toWorld(sx, sy);
    cam.x += before.x - after.x;
    cam.y += before.y - after.y;
    camTarget = null;
    request();
  }

  /** The buttons and keys: a smooth step about the stage centre. */
  function zoomStep(factor) {
    if (!world) return;
    const from = camTarget ?? cam;
    const target = { x: from.x, y: from.y, k: clamp(from.k * factor, kMin, K_MAX) };
    if (reduced) Object.assign(cam, target), (camTarget = null);
    else camTarget = target;
    request();
  }

  // ---------- semantic zoom ----------
  // A topic is open (its ideas and sub-topics drawn) or folded (one disc with
  // its count) by its block's size on screen, so a big topic stays open
  // further out than a small one and the rule needs no depth. `fade` eases
  // the switch; `shown` is how visible a topic's contents are (its own fade
  // times its parent's), `bubble` how visible its folded disc is.

  function opensAt(t, k) {
    if (k >= K_OPEN_ALL) return true;
    const px = Math.max(t.w, t.h) * k;
    return t.open ? px >= CLOSE_PX : px >= OPEN_PX;
  }

  /** Folds and unfolds topics for the current zoom; `snap` skips the fade. */
  function updateLod(snap = false) {
    fading = false;
    if (!world) return;
    for (const t of world.topics) {
      // Pre-order: a parent's `shown` is ready before its children read it.
      t.open = opensAt(t, cam.k);
      const goal = t.open ? 1 : 0;
      t.fade = snap || reduced || t.fade === undefined ? goal : t.fade + (goal - t.fade) * FADE;
      if (Math.abs(goal - t.fade) < 0.01) t.fade = goal;
      else fading = true;
      const outer = t.parent ? t.parent.shown : 1;
      t.shown = outer * t.fade;
      t.bubble = outer * (1 - t.fade);
    }
  }

  /** How visible an idea is: its home topic's contents. */
  const seen = (n) => n.cluster.topic.shown;
  /** Only an idea that is really there takes the pointer. */
  const live = (n) => seen(n) > 0.5;

  /** What an idea is drawn as: itself, or the outermost folded topic around it. */
  function repOf(n) {
    for (const t of n.cluster.topic.chain) if (!t.open) return t;
    return n;
  }

  /** Disc radius of a folded topic: grows with its ideas, never past its block. */
  function bubbleR(t) {
    const base = Math.min(54, 15 + 4.5 * Math.sqrt(t.nodes.length));
    return Math.max(12, Math.min(base, 0.5 * Math.max(t.w, t.h) * cam.k));
  }

  /** Where the folded topics are this frame: the mean of their ideas. */
  function placeBubbles() {
    bubbles = [];
    if (!world) return;
    for (const t of world.topics) {
      if (t.bubble <= 0.01 || !t.nodes.length) continue;
      let x = 0;
      let y = 0;
      for (const n of t.nodes) {
        x += n.x;
        y += n.y;
      }
      const p = toScreen(x / t.nodes.length, y / t.nodes.length);
      bubbles.push({ t, x: p.x, y: p.y, r: bubbleR(t), a: t.bubble });
    }
  }

  /** The folded topic under a stage point (the innermost when two overlap). */
  function bubbleAt(s) {
    let best = null;
    for (const b of bubbles) {
      if (b.a < 0.5 || Math.hypot(b.x - s.x, b.y - s.y) > b.r + 4) continue;
      if (!best || b.t.depth > best.t.depth) best = b;
    }
    return best;
  }

  /** The innermost topic block around a world point, or null. */
  function topicAt(w) {
    let best = null;
    for (const t of world?.topics ?? []) {
      if (t.shown < 0.5 && t.bubble < 0.5) continue;
      if (w.x < t.x0 - 40 || w.x > t.x1 + 40 || w.y < t.y0 - 40 || w.y > t.y1 + 40) continue;
      if (!best || t.depth > best.depth) best = t;
    }
    return best;
  }

  // ---------- drawing ----------

  function resize() {
    const r = root.getBoundingClientRect();
    if (!r.width || !r.height) return;
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    W = r.width;
    H = r.height;
    canvas.width = Math.round(W * dpr);
    canvas.height = Math.round(H * dpr);
    bg = ctx.createRadialGradient(W * 0.5, H * 0.32, 0, W * 0.5, H * 0.32, Math.max(W, H) * 0.8);
    bg.addColorStop(0, C.bg1);
    bg.addColorStop(1, C.bg0);
    if (!stars.length) {
      for (let i = 0; i < 140; i++) {
        const s = (i * 2654435761) >>> 0;
        stars.push({ u: (s % 10007) / 10007, v: ((s >>> 7) % 9973) / 9973, r: 0.4 + ((s >>> 3) % 7) / 7, a: 0.14 + ((s >>> 11) % 9) * 0.02 });
      }
    }
    limits();
    request();
  }

  function drawStars() {
    for (const s of stars) {
      // Parallax: the sky drifts at a fifth of the map's pan. Still: no twinkle.
      const x = (((s.u * W * 1.5 - cam.x * cam.k * 0.2) % W) + W) % W;
      const y = (((s.v * H * 1.5 - cam.y * cam.k * 0.2) % H) + H) % H;
      ctx.fillStyle = `rgba(240, 236, 228, ${s.a.toFixed(3)})`;
      ctx.fillRect(x, y, s.r, s.r);
    }
  }

  /**
   * The topic's recorded evidence, in the learner's words: how many graded
   * answers this topic produced and how many of them came back wrong. Both
   * numbers are per *topic* (the source never graded a claim), so they can
   * never be read as "this idea was checked".
   */
  function checksLine(checks) {
    if (!checks) return '';
    const correct = Number(checks.correct) || 0;
    const missed = Number(checks.missed) || 0;
    if (!correct && !missed) return '';
    return `${correct + missed} answers · ${missed} wrong`;
  }

  const levelWord = (v) => (typeof v === 'string' && v ? v[0].toUpperCase() + v.slice(1) : '');

  /**
   * "Reached: Use · Aim: Analyze" for one idea, from the newest plan's
   * `nodeLevels` and section targets. Words, not a fourth box state
   * (ui-design §7.7.6); empty when the plan says nothing about this idea.
   */
  function levelLine(key) {
    const info = learn?.levels?.[key];
    if (!info) return '';
    const parts = [];
    if (info.reached) parts.push(`Reached: ${levelWord(info.reached)}`);
    if (info.aim) parts.push(`Aim: ${levelWord(info.aim)}`);
    return parts.join(' · ');
  }

  /** The newest session's confidence line and parked frontier. */
  function renderLearning() {
    learning.replaceChildren();
    if (learn) {
      const confidence = calibrationSummary(learn.calibration);
      if (confidence) learning.append(h('p', 'map-calibration', `Your confidence: ${confidence}`));
      const items = learn.frontier ?? [];
      if (items.length) {
        const details = h('details', 'map-frontier');
        details.append(h('summary', 'map-frontier-title', 'Saved for later'));
        const ul = h('ul', 'map-frontier-list');
        for (const item of items) {
          const li = h('li');
          li.append(h('span', 'map-frontier-topic', item.topic));
          if (item.why) li.append(h('span', 'map-frontier-why', ` — ${item.why}`));
          const b = h('button', 'btn text map-frontier-start', 'Start a session');
          b.type = 'button';
          b.setAttribute('aria-label', `Start a session on ${item.topic}`);
          b.addEventListener('click', () => onStartFrontier?.(item.id, b));
          li.append(b);
          ul.append(li);
        }
        details.append(ul);
        learning.append(details);
      }
    }
    learning.hidden = !learning.childElementCount;
  }

  /**
   * Frames a cluster, a topic block or several of them at a readable zoom;
   * the map's own pan, nothing new. A folded topic opens on the way in.
   */
  function jumpTo(target) {
    const boxes = (Array.isArray(target) ? target : [target]).filter(Boolean);
    if (!world || !boxes.length) return;
    const frame = frameOf(boxes);
    if (reduced) Object.assign(cam, frame), (camTarget = null);
    else camTarget = frame;
    request();
  }

  /**
   * The guide, as a list of the topics that have a venue (ui-design §7.6):
   * "Mathematics › Calculus — 2". A row jumps to that topic so the guide is
   * reachable without opening an idea's card. A topic with venues but no ideas
   * on the map has no cluster to jump to, so it is not listed here; its parent
   * row still carries its venues.
   */
  function renderGuide() {
    guide.replaceChildren();
    if (!world || !venuesByTopic.size) {
      guide.hidden = true;
      return;
    }
    const rows = world.clusters
      .map((c) => ({ cluster: c, venues: venuesByTopic.get(c.id) ?? [] }))
      .filter((row) => row.venues.length);
    if (!rows.length) {
      guide.hidden = true;
      return;
    }
    const total = rows.length;
    const head = h(
      'summary',
      'map-guide-title',
      `Where the field lives · ${total} ${total === 1 ? 'topic' : 'topics'}`,
    );
    const ul = h('ul', 'map-guide-list');
    for (const { cluster, venues } of rows) {
      const li = h('li', 'map-guide-row');
      const b = h('button', 'map-guide-topic', cluster.path);
      b.type = 'button';
      b.setAttribute('aria-label', `Jump to ${cluster.path}`);
      b.addEventListener('click', () => jumpTo(cluster));
      li.append(b, h('span', 'map-guide-count', String(venues.length)));
      ul.append(li);
    }
    guide.append(head, ul);
    guide.hidden = false;
  }

  /**
   * Cluster titles are screen-space: pinned inside the stage at a fixed size,
   * so the whole-map view still says which topic sits where (a world-space
   * title shrinks to nothing at that zoom). A title reads as the heading of
   * the ideas under it, so it is only ever placed above its cluster, and
   * dropped rather than overprinted when that doesn't fit — zooming in
   * separates the clusters and brings it back. Only open topics have titles
   * here (a folded one is named under its disc, see drawBubbles); an open
   * topic that holds only sub-topics gets a heading of its own above its
   * block. `titleBox` / `headBox` are screen space: the click hit test
   * compares pointer coordinates.
   *
   * @returns the screen boxes taken, for the labels drawn after
   */
  function drawClusterTitles() {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const tfont = `600 16px ${FONT_READ}`;
    const sfont = `400 13px ${FONT_UI}`;
    const placed = [];
    const nodePts = world.nodes.filter(live).map((n) => toScreen(n.x, n.y));
    const discs = bubbles.filter((b) => b.a > 0.5);
    for (const c of world.clusters) c.titleBox = null;
    for (const t of world.topics) t.headBox = null;
    // Headings first (outermost first), then cluster titles (biggest first);
    // the keyboard's cluster always wins.
    const items = [
      ...world.topics
        .filter((t) => !t.cluster && t.children.length && t.shown > 0.01)
        .map((t) => ({ obj: t, path: t.path, sub: '', cx: t.cx, y0: t.y0, alpha: t.shown, rank: t.depth })),
      ...world.clusters
        .filter((c) => c.topic.shown > 0.01)
        .map((c) => ({ obj: c, path: c.path, sub: checksLine(c.checks), cx: c.cx, y0: c.y0, alpha: c.topic.shown, rank: 100 - c.nodes.length / 1000 })),
    ].sort((a, b) => (b.obj === focusCluster) - (a.obj === focusCluster) || a.rank - b.rank);
    for (const it of items) {
      const c = it.obj;
      // A block wholly beside the stage gets no title: one pinned to the edge
      // would name a topic that isn't there (islands sit far apart).
      if (toScreen(c.x1, 0).x < 0 || toScreen(c.x0, 0).x > W) continue;
      const sub = it.sub;
      ctx.font = tfont;
      c.lines ??= split(ctx, it.path, 360);
      const widest = Math.max(...c.lines.map((line) => ctx.measureText(line).width));
      ctx.font = sfont;
      const totalW = Math.max(widest, sub ? ctx.measureText(sub).width : 0);
      const lineH = 20;
      const height = c.lines.length * lineH + (sub ? 20 : 0) + 4;
      const top = toScreen(it.cx, it.y0).y;
      const x = clamp(toScreen(it.cx, it.y0).x, totalW / 2 + 10, Math.max(totalW / 2 + 10, W - totalW / 2 - 10));
      const boxFor = (y0) => ({ x0: x - totalW / 2, x1: x + totalW / 2, y0, y1: y0 + height });
      const hits = (b) => placed.some((p) => p.x0 < b.x1 && b.x0 < p.x1 && p.y0 < b.y1 && b.y0 < p.y1);
      // A title may not sit on another cluster's ideas, nor on a folded
      // topic's disc: the map's job at this zoom is to show where each topic
      // is, not to overprint it.
      const clears = (b) =>
        !nodePts.some((p) => p.x > b.x0 - 6 && p.x < b.x1 + 6 && p.y > b.y0 - 6 && p.y < b.y1 + 6) &&
        !discs.some((d) => d.x + d.r > b.x0 - 6 && d.x - d.r < b.x1 + 6 && d.y + d.r > b.y0 - 6 && d.y - d.r < b.y1 + 6);
      const candidates = [boxFor(top - 10 - height), boxFor(top - 10 - 2 * height - 8)].filter(
        (b) => b.y0 > 52 && b.y1 < H - 8,
      );
      const box = c === focusCluster ? (candidates[0] ?? null) : (candidates.find((b) => !hits(b) && clears(b)) ?? null);
      if (!box) continue;
      placed.push(box);
      // Clickable once it is really there, not while it fades.
      if (it.alpha > 0.5) {
        if (c.chain) c.headBox = box;
        else c.titleBox = box;
      }
      ctx.globalAlpha = forced ? 1 : it.alpha;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'top';
      ctx.fillStyle = forced ? 'CanvasText' : C.text;
      ctx.font = tfont;
      c.lines.forEach((line, i) => ctx.fillText(line, x, box.y0 + 2 + i * lineH));
      if (sub) {
        ctx.font = sfont;
        ctx.fillStyle = forced ? 'CanvasText' : C.textDim;
        ctx.fillText(sub, x, box.y0 + 2 + c.lines.length * lineH - 2);
      }
      ctx.textBaseline = 'alphabetic';
      if (c === focusCluster) {
        ctx.strokeStyle = forced ? 'Highlight' : C.focus;
        ctx.lineWidth = 2;
        ctx.strokeRect(box.x0 - 8, box.y0 - 4, box.x1 - box.x0 + 16, box.y1 - box.y0 + 6);
      }
      ctx.globalAlpha = 1;
    }
    return placed;
  }

  function drawEdge(e, alpha, dimOf, spot) {
    const dim = Math.min(dimOf(e.a), dimOf(e.b));
    const hot = spot && (e.a === spot || e.b === spot);
    const p = curve(e, 0);
    ctx.beginPath();
    ctx.moveTo(p.x0, p.y0);
    ctx.bezierCurveTo(p.x0, p.y1, p.x3, p.y2, p.x3, p.y3);
    ctx.strokeStyle = forced ? 'CanvasText' : hot ? C.edgeHot : C.edge;
    ctx.globalAlpha = forced ? 1 : dim * alpha;
    ctx.lineWidth = hot ? 1.9 : 1.3;
    ctx.stroke();
    // Arrowhead at the idea that needs it.
    const end = curve(e, 1);
    const len = Math.hypot(end.tx, end.ty) || 1;
    const ux = end.tx / len;
    const uy = end.ty / len;
    ctx.beginPath();
    ctx.moveTo(end.x, end.y);
    ctx.lineTo(end.x - ux * 8 - uy * 4, end.y - uy * 8 + ux * 4);
    ctx.lineTo(end.x - ux * 8 + uy * 4, end.y - uy * 8 - ux * 4);
    ctx.closePath();
    ctx.fillStyle = ctx.strokeStyle;
    ctx.fill();
    ctx.globalAlpha = 1;
  }

  /**
   * Edges between visible ideas are drawn as curves here; an edge with an
   * end inside a folded topic is counted toward the link between the two
   * things actually drawn (an idea or the outermost folded topic), returned
   * as from → to → count for drawLinks.
   */
  function drawEdges(dimOf, spot) {
    const links = new Map();
    for (const e of world.edges) {
      const alpha = Math.min(seen(e.a), seen(e.b));
      if (alpha > 0.01) drawEdge(e, alpha, dimOf, spot);
      const from = repOf(e.a);
      const to = repOf(e.b);
      if (from === to || (from === e.a && to === e.b)) continue;
      const row = links.get(from) ?? links.set(from, new Map()).get(from);
      row.set(to, (row.get(to) ?? 0) + 1);
    }
    return links;
  }

  /**
   * Prerequisites into, out of and between folded topics: one straight line
   * per pair and direction, thicker the more edges it stands for, in stage px
   * so it reads at any zoom. Each direction keeps to its own side, so A → B
   * and B → A stay two lines.
   */
  function drawLinks(links, spot) {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const discOf = new Map(bubbles.map((b) => [b.t, b]));
    const end = (rep) => {
      if (rep.chain) return discOf.get(rep) ?? null;
      const p = toScreen(rep.x, rep.y);
      return { x: p.x, y: p.y, r: rep.r * rep.scale * cam.k, a: seen(rep) };
    };
    for (const [from, row] of links) {
      for (const [to, count] of row) {
        const a = end(from);
        const b = end(to);
        if (!a || !b) continue;
        const alpha = Math.min(a.a, b.a);
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const len = Math.hypot(dx, dy);
        if (alpha <= 0.01 || len < a.r + b.r + 8) continue;
        // A gentle bow to the right of travel: A → B and B → A become two
        // arcs, and links through a row of discs clear the middle ones.
        const bow = Math.min(48, len * 0.15);
        const cx = (a.x + b.x) / 2 - (dy / len) * bow;
        const cy = (a.y + b.y) / 2 + (dx / len) * bow;
        const la = Math.hypot(cx - a.x, cy - a.y) || 1;
        const lb = Math.hypot(b.x - cx, b.y - cy) || 1;
        const x0 = a.x + ((cx - a.x) / la) * (a.r + 3);
        const y0 = a.y + ((cy - a.y) / la) * (a.r + 3);
        const ux = (b.x - cx) / lb;
        const uy = (b.y - cy) / lb;
        const x1 = b.x - ux * (b.r + 3);
        const y1 = b.y - uy * (b.r + 3);
        const hot = (hoverTopic && (from === hoverTopic || to === hoverTopic)) || (spot && (from === spot || to === spot));
        const width = Math.min(5, 1.2 + 1.1 * Math.log2(count)) + (hot ? 0.6 : 0);
        ctx.globalAlpha = forced ? 1 : alpha;
        ctx.strokeStyle = forced ? 'CanvasText' : hot ? C.linkHot : C.link;
        ctx.lineWidth = width;
        ctx.beginPath();
        ctx.moveTo(x0, y0);
        ctx.quadraticCurveTo(cx, cy, x1, y1);
        ctx.stroke();
        const head = 7 + width;
        ctx.beginPath();
        ctx.moveTo(x1, y1);
        ctx.lineTo(x1 - ux * head - uy * head * 0.5, y1 - uy * head + ux * head * 0.5);
        ctx.lineTo(x1 - ux * head + uy * head * 0.5, y1 - uy * head - ux * head * 0.5);
        ctx.closePath();
        ctx.fillStyle = ctx.strokeStyle;
        ctx.fill();
      }
    }
    ctx.globalAlpha = 1;
  }

  /**
   * Folded topics: a disc with the number of ideas inside, the topic's name
   * and "n ideas · m topics" under it. Names go outermost and biggest first;
   * one that would cover a title, an idea, another name or another disc is
   * left out (its disc still shows the count, and hovering names it).
   */
  function drawBubbles(placed) {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.textAlign = 'center';
    for (const b of bubbles) {
      const { t } = b;
      const r = b.r * (t === hoverTopic ? 1.08 : 1);
      ctx.globalAlpha = forced ? 1 : b.a;
      ctx.beginPath();
      ctx.arc(b.x, b.y, r, 0, Math.PI * 2);
      ctx.fillStyle = forced ? 'Canvas' : C.topicFill;
      ctx.fill();
      ctx.strokeStyle = forced ? 'CanvasText' : t === hoverTopic ? C.focus : C.topicRing;
      ctx.lineWidth = t === hoverTopic ? 2.2 : 1.6;
      ctx.stroke();
      // Some of its ideas are picked for practice: the selection ring, as on an idea.
      if (t.nodes.some((n) => selected.has(n.key))) {
        ctx.beginPath();
        ctx.arc(b.x, b.y, r + 6, 0, Math.PI * 2);
        ctx.strokeStyle = forced ? 'Highlight' : C.select;
        ctx.lineWidth = 2;
        ctx.setLineDash([7, 5]);
        ctx.stroke();
        ctx.setLineDash([]);
      }
      ctx.fillStyle = forced ? 'CanvasText' : C.text;
      ctx.font = `600 ${r >= 24 ? 15 : 12}px ${FONT_UI}`;
      ctx.textBaseline = 'middle';
      ctx.fillText(String(t.nodes.length), b.x, b.y + 1);
    }
    const nodePts = world.nodes.filter(live).map((n) => toScreen(n.x, n.y));
    const named = bubbles.filter((b) => b.a > 0.5).sort((p, q) => p.t.depth - q.t.depth || q.t.nodes.length - p.t.nodes.length);
    ctx.textBaseline = 'top';
    for (const b of named) {
      const { t } = b;
      const ideas = t.nodes.length;
      const sub = `${ideas} ${ideas === 1 ? 'idea' : 'ideas'}${t.subtopics ? ` · ${t.subtopics} ${t.subtopics === 1 ? 'topic' : 'topics'}` : ''}`;
      ctx.font = `600 14px ${FONT_READ}`;
      t.nameLines ??= wrap(ctx, t.title, 180);
      const nameW = Math.max(...t.nameLines.map((line) => ctx.measureText(line).width));
      ctx.font = `400 12px ${FONT_UI}`;
      const w = Math.max(nameW, ctx.measureText(sub).width);
      const y0 = b.y + b.r + 6;
      const box = { x0: b.x - w / 2, x1: b.x + w / 2, y0, y1: y0 + t.nameLines.length * 18 + 16 };
      const over = (p) => p.x0 < box.x1 && box.x0 < p.x1 && p.y0 < box.y1 && box.y0 < p.y1;
      const onDisc = (d) => d !== b && d.a > 0.5 && d.x + d.r > box.x0 && d.x - d.r < box.x1 && d.y + d.r > box.y0 && d.y - d.r < box.y1;
      const onIdea = (p) => p.x > box.x0 - 6 && p.x < box.x1 + 6 && p.y > box.y0 - 6 && p.y < box.y1 + 6;
      if (placed.some(over) || bubbles.some(onDisc) || nodePts.some(onIdea)) continue;
      placed.push(box);
      ctx.globalAlpha = forced ? 1 : b.a;
      ctx.fillStyle = forced ? 'CanvasText' : C.text;
      ctx.font = `600 14px ${FONT_READ}`;
      t.nameLines.forEach((line, i) => ctx.fillText(line, b.x, y0 + i * 18));
      ctx.fillStyle = forced ? 'CanvasText' : C.textDim;
      ctx.font = `400 12px ${FONT_UI}`;
      ctx.fillText(sub, b.x, y0 + t.nameLines.length * 18);
    }
    ctx.textBaseline = 'alphabetic';
    ctx.globalAlpha = 1;
  }

  function drawNodes(k, dimOf, spot, placed) {
    const labelNodes = [];
    for (const n of world.nodes) {
      const target = n === hover || n === focusNode ? 1.16 : 1;
      n.scale += (target - n.scale) * (reduced ? 1 : 0.2);
      const alpha = seen(n);
      if (alpha <= 0.01) continue;
      const r = n.r * n.scale;
      const dim = dimOf(n) * alpha;
      const kind = kindOf(n);

      ctx.globalAlpha = dim;
      ctx.beginPath();
      ctx.arc(n.x, n.y, r, 0, Math.PI * 2);
      if (forced) {
        ctx.fillStyle = 'Canvas';
        ctx.fill();
        ctx.strokeStyle = 'CanvasText';
        ctx.lineWidth = kind.ring / k;
        ctx.setLineDash(kind.dash);
        ctx.stroke();
        ctx.setLineDash([]);
      } else {
        ctx.fillStyle = kind.tint;
        ctx.fill();
        ctx.strokeStyle = kind.ink;
        ctx.lineWidth = kind.ring;
        ctx.setLineDash(kind.dash);
        ctx.stroke();
        ctx.setLineDash([]);
      }

      if (selected.has(n.key)) {
        ctx.beginPath();
        ctx.arc(n.x, n.y, r + 8, 0, Math.PI * 2);
        ctx.strokeStyle = forced ? 'Highlight' : C.select;
        ctx.lineWidth = 2.2;
        ctx.setLineDash([7, 5]);
        ctx.stroke();
        ctx.setLineDash([]);
      }
      if (n === focusNode) {
        ctx.beginPath();
        ctx.arc(n.x, n.y, r + (selected.has(n.key) ? 13 : 6), 0, Math.PI * 2);
        ctx.strokeStyle = forced ? 'Highlight' : C.focus;
        ctx.lineWidth = 2.5;
        ctx.stroke();
      }
      // Label candidates are drawn after the nodes (see below) so a dense
      // cluster cannot bury its neighbours in overprinted titles.
      if (alpha > 0.6 && (k >= 0.55 || n === spot || selected.has(n.key))) labelNodes.push(n);
      ctx.globalAlpha = 1;
    }

    // Labels last, decluttered: the idea in focus wins, then selected ideas,
    // then the rest in graph order; a label that would overlap one already
    // drawn, or a topic title, is dropped (the hover card still shows every
    // title in full). Boxes are compared in stage px, with the titles'.
    const rank = (n) => (n === spot ? 3 : selected.has(n.key) ? 2 : 1);
    labelNodes.sort((a, b) => rank(b) - rank(a));
    ctx.textAlign = 'center';
    ctx.textBaseline = 'top';
    for (const n of labelNodes) {
      const kind = kindOf(n);
      ctx.font = `${kind.weight} 13px ${FONT_UI}`;
      n.lines ??= wrap(ctx, n.title, 150);
      const widest = Math.max(...n.lines.map((line) => ctx.measureText(line).width));
      const r = n.r * n.scale;
      const a = toScreen(n.x - widest / 2, n.y + r + 6);
      const b = toScreen(n.x + widest / 2, n.y + r + 8 + n.lines.length * 16);
      const box = { x0: a.x, x1: b.x, y0: a.y, y1: b.y };
      if (placed.some((p) => p.x0 < box.x1 && box.x0 < p.x1 && p.y0 < box.y1 && box.y0 < p.y1)) continue;
      placed.push(box);
      ctx.globalAlpha = forced ? 1 : seen(n);
      ctx.fillStyle = forced ? 'CanvasText' : C.text;
      n.lines.forEach((line, i) => ctx.fillText(line, n.x, n.y + r + 8 + i * 16));
    }
    ctx.globalAlpha = 1;
  }

  function drawEffects(t) {
    if (forced || reduced) return;
    rings = rings.filter((ring) => t - ring.t < 0.6);
    for (const ring of rings) {
      const p = (t - ring.t) / 0.6;
      ctx.beginPath();
      ctx.arc(ring.x, ring.y, ring.r + 46 * easeOut(p), 0, Math.PI * 2);
      ctx.strokeStyle = ring.color;
      ctx.globalAlpha = 1 - p;
      ctx.lineWidth = 2.5 * (1 - p) + 0.5;
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = 'lighter';
    for (const s of sparks) {
      const a = 1 - s.life / s.max;
      ctx.globalAlpha = Math.max(0, a);
      const size = 5 + 7 * a;
      const spriteAt = glow[s.color] ?? glow.white;
      ctx.drawImage(spriteAt, s.x - size / 2, s.y - size / 2, size, size);
    }
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = 'source-over';
  }

  /** The topic a dragged idea would move to: a dashed outline round its ideas. */
  function drawDropTarget(k) {
    const c = drag?.node && drag.target;
    if (!c) return;
    ctx.save();
    ctx.strokeStyle = forced ? 'Highlight' : C.select;
    ctx.lineWidth = 2 / k;
    ctx.setLineDash([8 / k, 6 / k]);
    const pad = 18;
    ctx.strokeRect(c.x0 - pad, c.y0 - pad, c.w + 2 * pad, c.h + 2 * pad);
    ctx.restore();
  }

  function draw(t) {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    if (forced) {
      ctx.fillStyle = 'Canvas';
      ctx.fillRect(0, 0, W, H);
    } else {
      ctx.fillStyle = bg;
      ctx.fillRect(0, 0, W, H);
      drawStars();
    }
    if (!world) return;
    const k = cam.k;

    placeBubbles();
    const placed = drawClusterTitles();
    ctx.setTransform(dpr * k, 0, 0, dpr * k, dpr * (W / 2 - cam.x * k), dpr * (H / 2 - cam.y * k));

    const spot = hover ?? focusNode;
    const near = spot ? new Set([spot, ...spot.needs, ...spot.leads]) : null;
    const dimOf = (n) => (near && !near.has(n) ? 0.28 : 1);

    const links = drawEdges(dimOf, spot);
    drawDropTarget(k);
    drawNodes(k, dimOf, spot, placed);
    drawEffects(t);
    drawLinks(links, spot);
    drawBubbles(placed);
  }

  // ---------- card and flyout ----------

  function cancelHide() {
    clearTimeout(hat);
    hat = 0;
  }

  function hideSoon() {
    if (hat) return; // moving within the node/card/flyout keeps the same timer
    hat = setTimeout(() => {
      hat = 0;
      if (focusNode) return; // the keyboard leads, the mouse spot was only hovering
      hover = null;
      closeFlyout();
      request();
    }, HIDE_MS);
  }

  /**
   * Zero is "not recorded", never "depends on nothing": the store only holds
   * the prerequisites its extractor chained (25 chains over 49 claims here), so
   * a claim with no incoming edge may well have prerequisites the graph has not
   * captured yet. The label says which, because the difference is the learner's
   * trust in the map.
   */
  function linkLabel(dir, count) {
    if (!count) return dir === 'needs' ? 'No prerequisite recorded' : 'Nothing recorded depends on this';
    return `${dir === 'needs' ? 'Depends on' : 'Leads to'} (${count})`;
  }

  function cardLink(node, dir, count) {
    const b = h('button', 'map-card-link', linkLabel(dir, count));
    b.type = 'button';
    if (!count) b.dataset.empty = 'true';
    b.addEventListener('pointerenter', () => {
      cancelHide();
      openFlyout(node, dir);
    });
    b.addEventListener('focus', () => {
      cancelHide();
      if (!focusNode) focusNode = hover;
      openFlyout(node, dir);
    });
    b.addEventListener('click', (e) => {
      e.preventDefault();
      openFlyout(node, dir);
    });
    return b;
  }

  /**
   * The topic's guide on an idea's card: its own venues plus its descendants'
   * (a venue under `Mathematics › Calculus` is on `Mathematics` too), as the
   * backend aggregated them. Empty when the topic has none.
   */
  function venueSection(topicId) {
    const list = venuesByTopic.get(topicId) ?? [];
    if (!list.length) return null;
    const wrap = h('div', 'map-card-venues');
    wrap.append(h('p', 'map-card-venues-title', 'Where the field lives'));
    wrap.append(
      venueGroups(list, (url) => call('open_external', { url }).catch(() => {}), { entry: true }),
    );
    return wrap;
  }

  function buildCard(n) {
    const title = h('p', 'map-card-title', n.title);
    const meta = h('p', 'map-card-meta', `${kindName(n)} · ${n.cluster.path}`);
    const level = levelLine(n.key);
    const statement = h('p', 'map-card-statement', n.statement || 'No description recorded.');
    const similarities = contrastTitles.get(n.key) ?? [];
    const links = h('p', 'map-card-links');
    links.append(
      cardLink(n, 'needs', n.needs.length),
      h('span', 'map-card-sep', '·'),
      cardLink(n, 'leads', n.leads.length),
    );
    card.replaceChildren(title, meta);
    if (level) card.append(h('p', 'map-card-level', level));
    card.append(statement);
    // A contrast is a note, not a relationship with a direction: it never gets
    // a "Depends on"/"Leads to" label or an arrow.
    if (similarities.length) {
      card.append(h('p', 'map-card-contrast', `Contrasts with ${similarities.join(', ')}`));
    }
    card.append(links);
    const places = venueSection(n.cluster.id);
    if (places) card.append(places);
    card.append(originSection(n));
    const move = h('button', 'map-card-move', 'Move to another topic…');
    move.type = 'button';
    move.addEventListener('click', () => openMove(n));
    card.append(move);
  }

  function placeCard() {
    // A hovered idea that zooming folded away takes its card with it.
    if (hover && !live(hover)) hover = null;
    const n = hover ?? focusNode;
    if (!n || (drag && drag.moved)) {
      card.hidden = true;
      closeFlyout();
      return;
    }
    if (cardFor !== n.key) {
      cardFor = n.key;
      // A flyout opened from the keyboard is requested before the card exists;
      // keep it when it belongs to this idea, drop it when the idea changed.
      if (flyoutFor?.node !== n) closeFlyout();
      buildCard(n);
    }
    card.hidden = false;
    const p = toScreen(n.x, n.y);
    const r = n.r * cam.k;
    const cw = card.offsetWidth || 260;
    const ch = card.offsetHeight || 120;
    const x = p.x + r + 16 + cw <= W - 8 ? p.x + r + 16 : Math.max(8, p.x - r - 16 - cw);
    const y = clamp(p.y - 28, 8, Math.max(8, H - ch - 8));
    card.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
    if (flyoutFor && !flyout.hidden) placeFlyout();
  }

  function placeFlyout() {
    if (card.hidden) return;
    const c = card.getBoundingClientRect();
    const stage = root.getBoundingClientRect();
    const fw = flyout.offsetWidth;
    const fh = flyout.offsetHeight;
    const px = c.left - stage.left;
    const py = c.top - stage.top;
    let x = px + c.width + 12;
    let y = py;
    if (x + fw > W - 8) x = px - fw - 12; // then left of the card
    if (x < 8) {
      // then below it, clamped to the stage
      x = clamp(px, 8, Math.max(8, W - fw - 8));
      y = py + c.height + 10;
    }
    y = clamp(y, 8, Math.max(8, H - fh - 8));
    flyout.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
  }

  function openFlyout(node, dir) {
    const list = dir === 'needs' ? node.needs : node.leads;
    if (!list.length) return;
    if (flyoutFor && flyoutFor.node === node && flyoutFor.dir === dir && !flyout.hidden) return;
    cancelHide();
    flyoutFor = { node, dir };
    const head = h('p', 'map-flyout-title', dir === 'needs' ? 'Depends on' : 'Leads to');
    const ul = h('ul', 'map-flyout-list');
    for (const x of list) ul.append(h('li', '', x.title));
    flyout.replaceChildren(head, ul);
    flyout.hidden = false;
    placeFlyout();
    request();
  }

  function closeFlyout() {
    flyoutFor = null;
    flyout.hidden = true;
  }

  function updateStats(t) {
    const p = reduced ? 1 : easeOut(clamp(t / 1.1, 0, 1));
    for (const [k, el] of Object.entries(statEls)) {
      const v = String(Math.round(counts[k] * p));
      if (el.textContent !== v) el.textContent = v;
    }
  }

  /** True while a frame would change something on screen. */
  function moving() {
    if (camTarget || sparks.length || rings.length || fading) return true;
    if (!world) return false;
    if (!reduced && active(world)) return true;
    const spot = hover ?? focusNode;
    for (const n of world.nodes) if (Math.abs(n.scale - (n === spot ? 1.16 : 1)) > 0.005) return true;
    return false;
  }

  function frame(ts) {
    raf = 0;
    if (!visible()) return;
    const t = (ts - t0) / 1000;
    const dt = Math.min(0.05, t - lastT || 0.016);
    lastT = t;
    if (world) {
      if (!reduced) tick(world);
      if (camTarget) {
        // Zoom eases in log space, so a long zoom-in (whole map → one topic)
        // doesn't rush its first frames; it ends exactly on the target.
        const e = 0.14;
        cam.x += (camTarget.x - cam.x) * e;
        cam.y += (camTarget.y - cam.y) * e;
        cam.k = Math.exp(Math.log(cam.k) + (Math.log(camTarget.k) - Math.log(cam.k)) * e);
        if (Math.abs(camTarget.x - cam.x) * cam.k < 0.5 && Math.abs(camTarget.y - cam.y) * cam.k < 0.5 && Math.abs(camTarget.k / cam.k - 1) < 0.002) {
          Object.assign(cam, camTarget);
          camTarget = null;
        }
      }
      updateLod();
      for (const s of sparks) {
        s.life += dt;
        s.x += s.vx;
        s.y += s.vy;
        s.vx *= 0.94;
        s.vy = s.vy * 0.94 + 0.05;
      }
      sparks = sparks.filter((s) => s.life < s.max);
      updateStats(t);
    }
    draw(t);
    placeCard();
    // Only keep drawing while something actually moves; edges are still.
    if (moving()) request();
  }

  function request() {
    if (!raf && visible()) raf = requestAnimationFrame(frame);
  }

  document.addEventListener('visibilitychange', request);
  new ResizeObserver(resize).observe(root);

  // ---------- pointer ----------

  function local(e) {
    const r = canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  canvas.addEventListener('pointerdown', (e) => {
    if (!world) return;
    canvas.setPointerCapture(e.pointerId);
    const s = local(e);
    pointers.set(e.pointerId, s);
    if (pointers.size === 2) {
      // Two fingers: zoom about their midpoint, which also pans with them.
      const [a, b] = [...pointers.values()];
      const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
      pinch = { d: Math.hypot(a.x - b.x, a.y - b.y) || 1, k: cam.k, at: toWorld(mid.x, mid.y) };
      if (drag?.node) drag.node.fx = drag.node.fy = null;
      drag = null;
      camTarget = null;
      return;
    }
    const w = toWorld(s.x, s.y);
    const bubble = bubbleAt(s);
    const n = bubble ? null : hit(world, w.x, w.y, 6, live);
    camTarget = null;
    if (n) {
      n.fx = n.x;
      n.fy = n.y;
      drag = { node: n, sx: s.x, sy: s.y, moved: false, vx: 0, vy: 0, last: s, lastT: e.timeStamp };
      reheat(world, 0.3);
    } else {
      // A folded topic pans like the background; a click (no drag) opens it.
      drag = { pan: true, topic: bubble?.t ?? null, sx: s.x, sy: s.y, cx: cam.x, cy: cam.y, moved: false };
    }
    request();
  });

  canvas.addEventListener('pointermove', (e) => {
    if (!world) return;
    const s = local(e);
    if (pointers.has(e.pointerId)) pointers.set(e.pointerId, s);
    if (pinch && pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
      cam.k = clamp((pinch.k * Math.hypot(a.x - b.x, a.y - b.y)) / pinch.d, kMin, K_MAX);
      // Keep the world point that was under the midpoint under it.
      cam.x = pinch.at.x - (mid.x - W / 2) / cam.k;
      cam.y = pinch.at.y - (mid.y - H / 2) / cam.k;
      request();
      return;
    }
    if (drag) {
      if (Math.hypot(s.x - drag.sx, s.y - drag.sy) > 4) drag.moved = true;
      if (drag.pan) {
        cam.x = drag.cx - (s.x - drag.sx) / cam.k;
        cam.y = drag.cy - (s.y - drag.sy) / cam.k;
      } else {
        const w = toWorld(s.x, s.y);
        const dt = Math.max(1, e.timeStamp - drag.lastT);
        drag.vx = ((s.x - drag.last.x) / cam.k / dt) * 16;
        drag.vy = ((s.y - drag.last.y) / cam.k / dt) * 16;
        drag.last = s;
        drag.lastT = e.timeStamp;
        drag.node.fx = w.x;
        drag.node.fy = w.y;
        if (reduced) {
          drag.node.x = w.x;
          drag.node.y = w.y;
        }
        // Over another topic, the drop will ask to move the idea there.
        drag.target = drag.moved ? dropTarget(w, drag.node) : null;
        reheat(world, 0.25);
      }
      request();
      return;
    }
    const bubble = bubbleAt(s);
    const topic = bubble?.t ?? null;
    if (topic !== hoverTopic) {
      hoverTopic = topic;
      // The disc's full name, for when its label had no room.
      canvas.title = topic ? `${topic.path} — ${topic.nodes.length} ${topic.nodes.length === 1 ? 'idea' : 'ideas'}. Click to zoom in.` : '';
      request();
    }
    const w = toWorld(s.x, s.y);
    const n = topic ? null : hit(world, w.x, w.y, 6, live);
    if (n) {
      cancelHide();
      if (n !== hover) {
        hover = n;
        closeFlyout();
      }
      canvas.style.cursor = 'pointer';
      request();
    } else {
      canvas.style.cursor = topic ? 'pointer' : 'grab';
      if (hover) hideSoon();
    }
  });

  const inBox = (s, b) => b && s.x >= b.x0 && s.x <= b.x1 && s.y >= b.y0 && s.y <= b.y1;

  function endPointer(e) {
    pointers.delete(e.pointerId);
    if (pinch) {
      if (pointers.size < 2) pinch = null;
      return;
    }
    if (!drag) return;
    const d = drag;
    drag = null;
    if (d.node) {
      const n = d.node;
      n.fx = n.fy = null;
      if (d.moved && d.target && e.type === 'pointerup') {
        // Dropped on another topic: nothing moves until the learner says so.
        // The idea floats back home while the dialog asks.
        n.vx = n.vy = 0;
        reheat(world, 0.3);
        if (reduced) {
          n.x = n.homeX;
          n.y = n.homeY;
        }
        openMove(n, d.target.id);
      } else if (d.moved) {
        // Fling: keep the hand's speed, then let the springs bring it home.
        n.vx = clamp(d.vx, -20, 20);
        n.vy = clamp(d.vy, -20, 20);
        if (reduced) {
          n.homeX = n.x;
          n.homeY = n.y;
        }
        reheat(world, 0.3);
      } else if (e.type === 'pointerup') toggleNode(n);
    } else if (!d.moved && e.type === 'pointerup') {
      const s = local(e);
      if (d.topic) jumpTo(d.topic);
      else {
        const c = world.clusters.find((cl) => inBox(s, cl.titleBox));
        const t = c ? null : world.topics.find((tp) => inBox(s, tp.headBox));
        if (c) toggleCluster(c);
        else if (t) jumpTo(t);
      }
    }
    request();
  }
  canvas.addEventListener('pointerup', endPointer);
  canvas.addEventListener('pointercancel', endPointer);
  canvas.addEventListener('pointerleave', () => {
    if (!drag && hover) hideSoon();
    if (hoverTopic) {
      hoverTopic = null;
      canvas.title = '';
      request();
    }
  });

  // ---------- zoom: the map's, never the page's ----------
  // Every zoom gesture over the stage zooms the map about the pointer and is
  // cancelled for the page. A trackpad pinch arrives as ctrl+wheel in
  // Chromium and WebKit, a mouse ctrl+wheel the same way, so both are caught
  // on the whole stage (HUD, card and buttons included), not only the canvas;
  // a plain wheel zooms over the canvas and scrolls whatever else it is over
  // (the card's long statement). Listeners are non-passive so preventDefault
  // holds. See docs/ui-design.md §7.6 "Zoom belongs to the map".
  root.addEventListener(
    'wheel',
    (e) => {
      const pinchy = e.ctrlKey || e.metaKey;
      if (e.target !== canvas && !pinchy) return;
      e.preventDefault();
      if (!world) return;
      const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? H : 1;
      const dy = e.deltaY * unit;
      // A pinch reports small deltas (≈ 1 per percent); a wheel notch ~100.
      const step = pinchy ? clamp(dy, -25, 25) * 0.01 : clamp(dy, -200, 200) * 0.0015;
      const s = local(e);
      zoomAt(Math.exp(-step), s.x, s.y);
    },
    { passive: false },
  );

  // Safari / WKWebView (macOS trackpad): pinch arrives as gesture events.
  // On iOS the same gesture also arrives as two pointers, handled above.
  let gesture = null;
  const gestureStart = (e) => {
    e.preventDefault();
    gesture = world && pointers.size < 2 ? { k: cam.k, s: local(e) } : null;
  };
  const gestureChange = (e) => {
    e.preventDefault();
    if (gesture && pointers.size < 2) zoomAt((gesture.k * e.scale) / cam.k, gesture.s.x, gesture.s.y);
  };
  const gestureEnd = (e) => {
    e.preventDefault();
    gesture = null;
  };
  root.addEventListener('gesturestart', gestureStart, { passive: false });
  root.addEventListener('gesturechange', gestureChange, { passive: false });
  root.addEventListener('gestureend', gestureEnd, { passive: false });

  // WebKitGTK (Linux) magnifies the whole page on a touchpad pinch before the
  // page sees any event. The app shell takes that gesture over and forwards
  // it as `app-pinch` window events (crates/app/src/pinch.rs); the pinch is
  // the map's when it begins over the stage, with nothing on top of it.
  let nativePinch = null;
  window.addEventListener('app-pinch', (e) => {
    const { phase, scale, x, y } = e.detail ?? {};
    if (phase === 'begin') {
      nativePinch = null;
      if (!world || !visible() || pointers.size >= 2) return;
      if (!root.contains(document.elementFromPoint(x, y))) return;
      const r = canvas.getBoundingClientRect();
      nativePinch = { k: cam.k, s: { x: x - r.left, y: y - r.top } };
    } else if (phase === 'update' && nativePinch && Number.isFinite(scale) && scale > 0) {
      zoomAt((nativePinch.k * scale) / cam.k, nativePinch.s.x, nativePinch.s.y);
    } else if (phase === 'end') nativePinch = null;
  });

  // Keys, while focus is in the stage (the idea list, the buttons): + and −
  // step the zoom, 0 fits; with ctrl/⌘ too, so the page's own zoom keys
  // zoom the map here instead of the app.
  root.addEventListener('keydown', (e) => {
    if (!world || e.altKey) return;
    const step = { '+': 1.25, '=': 1.25, '-': 0.8, _: 0.8 }[e.key];
    if (step) zoomStep(step);
    else if (e.key === '0') fit(false);
    else return;
    e.preventDefault();
  });

  /** True when the camera already frames `box` (a second double-click steps out). */
  function framing(box) {
    const f = frameOf([box]);
    return Math.abs(Math.log(f.k / cam.k)) < 0.15 && Math.hypot(f.x - cam.x, f.y - cam.y) * cam.k < 60;
  }

  // Double-click a topic (open or folded) to frame it; again to step out to
  // its parent; on open water, or from a root, to frame the whole map.
  canvas.addEventListener('dblclick', (e) => {
    if (!world) return;
    const s = local(e);
    const w = toWorld(s.x, s.y);
    if (hit(world, w.x, w.y, 6, live)) return;
    // A title or heading click already acted (select / open); don't also fit.
    if (world.clusters.some((c) => inBox(s, c.titleBox)) || world.topics.some((t) => inBox(s, t.headBox))) return;
    let t = bubbleAt(s)?.t ?? topicAt(w);
    while (t && framing(t)) t = t.parent;
    if (t) jumpTo(t);
    else fit(false);
  });
  fitButton.addEventListener('click', () => fit(false));
  zoomInButton.addEventListener('click', () => zoomStep(1.4));
  zoomOutButton.addEventListener('click', () => zoomStep(1 / 1.4));
  currentButton.addEventListener('click', () => {
    const boxes = currentClusters();
    if (boxes.length) jumpTo(boxes);
  });

  /** The clusters holding the newest session's ideas (`learn.levels`). */
  function currentClusters() {
    const out = new Set();
    for (const key of Object.keys(learn?.levels ?? {})) {
      const node = entries.get(key)?.node;
      if (node) out.add(node.cluster);
    }
    return [...out];
  }

  card.addEventListener('pointerenter', cancelHide);
  card.addEventListener('pointerleave', () => {
    if (hover || focusNode) hideSoon();
  });
  card.addEventListener('focusin', () => {
    cancelHide();
    if (!focusNode) focusNode = hover;
  });
  card.addEventListener('focusout', (e) => {
    if (card.contains(e.relatedTarget) || a11y.contains(e.relatedTarget)) return;
    focusNode = null;
    closeFlyout();
    request();
  });
  flyout.addEventListener('pointerenter', cancelHide);
  flyout.addEventListener('pointerleave', () => {
    if (hover || focusNode) hideSoon();
  });

  // ---------- keyboard (the hidden checkbox list) ----------

  a11y.addEventListener('change', (e) => {
    const t = e.target;
    if (!t.dataset.key) return;
    set(t.dataset.key, t.checked);
    const entry = entries.get(t.dataset.key);
    if (entry) burst(entry.node, t.checked ? 'white' : KIND[entry.node.kind] ? entry.node.kind : 'white', t.checked ? 22 : 10);
    changed();
    request();
  });

  a11y.addEventListener('focusin', (e) => {
    const node = e.target.dataset?.key ? entries.get(e.target.dataset.key)?.node : null;
    if (!node) return;
    hover = null; // the keyboard leads now; the mouse spot would hide the focus
    focusNode = node;
    focusCluster = node.cluster;
    if (e.target.dataset.link) openFlyout(node, e.target.dataset.link);
    else closeFlyout();
    const p = toScreen(node.x, node.y);
    if (p.x < 80 || p.x > W - 80 || p.y < 140 || p.y > H - 60) {
      camTarget = { x: node.x, y: node.y, k: Math.max(cam.k, 0.8) };
    }
    request();
  });
  a11y.addEventListener('focusout', (e) => {
    if (a11y.contains(e.relatedTarget) || card.contains(e.relatedTarget)) return;
    focusNode = null;
    focusCluster = null;
    closeFlyout();
    request();
  });

  // ---------- render ----------

  function statItem(key, word) {
    const span = h('span', 'map-stat');
    const num = h('b', 'map-num', '0');
    statEls[key] = num;
    span.append(num, ` ${word}`);
    return span;
  }

  function a11yList() {
    const out = [];
    for (const cluster of world.clusters) {
      const fs = h('fieldset');
      fs.append(h('legend', '', cluster.path));
      for (const n of cluster.nodes) {
        const label = h('label');
        const input = h('input');
        input.type = 'checkbox';
        input.dataset.key = n.key;
        const descId = `map-desc-${++uid}`;
        input.setAttribute('aria-describedby', descId);
        label.append(input, ` ${n.title}, ${kindName(n)}`);
        const needs = n.needs.map((x) => x.title);
        const level = levelLine(n.key);
        const similar = contrastTitles.get(n.key) ?? [];
        const desc = h(
          'span',
          '',
          `${needs.length ? `Depends on: ${needs.join(', ')}.` : 'No prerequisite recorded.'}${
            similar.length ? ` Contrasts with: ${similar.join(', ')}.` : ''
          }${level ? ` ${level}.` : ''}`,
        );
        desc.id = descId;
        desc.hidden = true;
        const links = h('span', 'map-a11y-links');
        for (const dir of ['needs', 'leads']) {
          const count = (dir === 'needs' ? n.needs : n.leads).length;
          const b = h('button', 'map-card-link', linkLabel(dir, count));
          b.type = 'button';
          b.dataset.key = n.key;
          b.dataset.link = dir;
          if (!count) b.disabled = true;
          links.append(b);
        }
        // The card's own controls, reachable without a pointer: where the idea
        // came from (focus moves into the card) and moving it.
        const origin = h('button', 'map-card-link', `Where “${n.title}” came from`);
        origin.type = 'button';
        origin.addEventListener('click', () => {
          hover = null;
          focusNode = n;
          cardFor = null;
          placeCard();
          card.querySelector('.map-card-origin-title')?.focus({ preventScroll: true });
        });
        const move = h('button', 'map-card-link', `Move “${n.title}” to another topic…`);
        move.type = 'button';
        move.addEventListener('click', () => openMove(n));
        links.append(origin, move);
        fs.append(label, desc, links);
        entries.set(n.key, { input, node: n });
      }
      out.push(fs);
    }
    return out;
  }

  /** Groups the graph's ideas by home topic, in the topic tree's order. */
  function clusterGroups(topics, nodes, topicChecks) {
    const topicById = new Map(topics.map((t) => [t.id, t]));
    const pathOf = (id) => {
      const parts = [];
      const seen = new Set();
      let cur = topicById.get(id);
      while (cur && !seen.has(cur.id)) {
        seen.add(cur.id);
        parts.unshift(cur.title || cur.id);
        cur = topicById.get(cur.parent);
      }
      return parts.length ? parts.join(' › ') : id ? String(id) : 'Unfiled';
    };
    const kids = new Map();
    for (const t of topics) {
      const parent = t.parent ?? null;
      if (!kids.has(parent)) kids.set(parent, []);
      kids.get(parent).push(t.id);
    }
    const order = [];
    const seen = new Set();
    const walk = (id) => {
      if (seen.has(id)) return;
      seen.add(id);
      order.push(id);
      for (const child of kids.get(id) ?? []) walk(child);
    };
    for (const root of kids.get(null) ?? []) walk(root);

    const byHome = new Map();
    for (const n of nodes) {
      const home = n.home ?? '';
      if (!byHome.has(home)) byHome.set(home, []);
      byHome.get(home).push(n);
    }
    const groups = [];
    for (const id of order) {
      const list = byHome.get(id);
      if (!list?.length) continue;
      groups.push({ id, path: pathOf(id), checks: topicChecks[id] ?? null, nodes: list });
      byHome.delete(id);
    }
    // A node whose home is not in the topic list still gets a cluster.
    for (const [id, list] of byHome) groups.push({ id, path: pathOf(id), checks: topicChecks[id] ?? null, nodes: list });
    return groups;
  }

  /**
   * Idea id → titles of the ideas it is easily confused with, from `contrasts`
   * edges only. The reading is symmetric (a note on both cards), and nothing
   * here ever becomes a link, an arrow or a layout input.
   */
  function contrastIndex(edges, nodes) {
    const titleOfId = new Map(nodes.map((n) => [n.id, n.title]));
    const out = new Map();
    for (const e of edges) {
      if (!isContrast(e)) continue;
      const from = titleOfId.get(e.from);
      const to = titleOfId.get(e.to);
      if (from === undefined || to === undefined) continue;
      const note = (self, other) => {
        const list = out.get(self) ?? [];
        if (!list.includes(other)) list.push(other);
        out.set(self, list);
      };
      note(e.from, to);
      note(e.to, from);
    }
    return out;
  }

  /** Topic id → its full path (`Mathematics › Calculus › Limits`). */
  function pathsOf(topics) {
    const byId = new Map(topics.map((t) => [t.id, t]));
    const out = new Map();
    for (const t of topics) {
      const parts = [];
      const seen = new Set();
      for (let cur = t; cur && !seen.has(cur.id); cur = byId.get(cur.parent)) {
        seen.add(cur.id);
        parts.unshift(cur.title || cur.id);
      }
      out.set(t.id, parts.join(' › '));
    }
    return out;
  }

  /**
   * Renders the map; the selection keeps the ideas that still exist.
   * `splitsIn` is `graph_splits`: the topic splits awaiting a decision.
   */
  function render(graph, reviewIn = null, learningIn = null, splitsIn = []) {
    origins = new Map();
    topicPaths = pathsOf(graph?.topics ?? []);
    splitReview = (splitsIn ?? []).filter((p) => topicPaths.has(p.topic) && (p.groups ?? []).length);
    drawSplits();
    reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
    forced = matchMedia('(forced-colors: active)').matches;
    if (forced) reduced = true;
    learn = learningIn;
    renderLearning();
    cancelHide();
    entries = new Map();
    sparks = [];
    rings = [];
    hover = focusNode = focusCluster = hoverTopic = null;
    bubbles = [];
    cardFor = null;
    flyoutFor = null;
    card.hidden = true;
    flyout.hidden = true;
    const topics = graph?.topics ?? [];
    const nodes = graph?.nodes ?? [];
    const edges = graph?.edges ?? [];
    venuesByTopic = new Map(Object.entries(graph?.topicVenues ?? {}));
    // Only a prerequisite orders the two ideas: it is the arrow, the flyout and
    // the layout input. A contrast is a remark on the cards (below), so it is
    // filtered out here rather than drawn as a dependency.
    const links = edges.filter(isRequires);
    contrastTitles = contrastIndex(edges, nodes);
    if (!nodes.length) {
      world = null;
      stats.replaceChildren();
      renderGuide();
      a11y.replaceChildren();
      // No idea to point at, so the card must not keep the last graph's text
      // (venue lists included) in the DOM.
      card.replaceChildren();
      fitButton.hidden = true;
      zoomBox.hidden = true;
      status.replaceChildren(h('p', 'map-status-text', EMPTY));
      selected.clear();
      changed();
      request();
      return;
    }
    status.replaceChildren();
    fitButton.hidden = false;
    zoomBox.hidden = false;
    world = createWorld(clusterGroups(topics, nodes, graph?.topicChecks ?? {}), links, { burst: !reduced, topics });
    if (reduced) settle(world);
    renderGuide();
    a11y.replaceChildren(...a11yList());
    for (const key of [...selected]) if (!entries.has(key)) selected.delete(key);
    for (const key of selected) entries.get(key).input.checked = true;

    counts = { ideas: world.nodes.length, topics: world.clusters.length, links: world.edges.length };
    statEls = {};
    const sep = () => Object.assign(h('span', 'map-stat-sep', '·'), { ariaHidden: 'true' });
    stats.replaceChildren(
      statItem('ideas', counts.ideas === 1 ? 'idea' : 'ideas'),
      sep(),
      statItem('topics', counts.topics === 1 ? 'topic' : 'topics'),
      sep(),
      statItem('links', counts.links === 1 ? 'recorded prerequisite' : 'recorded prerequisites'),
    );
    review = reviewIn ?? { proposals: [], titles: {} };
    drawProposals();

    t0 = performance.now();
    lastT = 0;
    resize();
    fit(true);
    updateLod(true);
    currentButton.hidden = !currentClusters().length;
    changed();
    request();
  }

  function loading() {
    world = null;
    splitReview = [];
    drawSplits();
    learn = null;
    venuesByTopic = new Map();
    renderLearning();
    renderGuide();
    cancelHide();
    stats.replaceChildren();
    a11y.replaceChildren();
    card.hidden = true;
    closeFlyout();
    fitButton.hidden = true;
    zoomBox.hidden = true;
    status.replaceChildren(h('p', 'map-status-text', 'Loading…'));
    request();
  }

  function failed(text, retry) {
    world = null;
    splitReview = [];
    drawSplits();
    learn = null;
    venuesByTopic = new Map();
    renderLearning();
    renderGuide();
    cancelHide();
    stats.replaceChildren();
    a11y.replaceChildren();
    card.hidden = true;
    closeFlyout();
    fitButton.hidden = true;
    zoomBox.hidden = true;
    const p = h('p', 'map-status-text danger', `Couldn’t load the map: ${text} `);
    p.setAttribute('role', 'status');
    const b = h('button', 'btn text', 'Try again');
    b.type = 'button';
    b.addEventListener('click', retry);
    p.append(b);
    status.replaceChildren(p);
    request();
  }

  function clear() {
    for (const key of [...selected]) set(key, false);
    selected.clear();
    changed();
    request();
  }

  /** The selected ideas in display order: { title, topic } per idea. */
  function selection() {
    const out = [];
    for (const [key, { node }] of entries) {
      if (selected.has(key)) out.push({ title: node.title, topic: node.cluster.path });
    }
    return out;
  }

  return { render, loading, failed, clear, selection };
}
