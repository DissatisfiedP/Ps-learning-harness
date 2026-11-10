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
const K_MIN = 0.3;
const K_MAX = 2.4;
const HIDE_MS = 150;

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
export function createMap(root, { onChange, onReviewDecided = null, onStartFrontier = null }) {
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
  root.replaceChildren(canvas, hud, proposalsButton, fitButton, a11y, card, flyout, status);
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
  let statEls = {};
  let hat = 0; // hide timer
  let cardFor = null;
  let flyoutFor = null;
  let review = { proposals: [], titles: {} };
  let decisionBusy = false;
  /** Idea id → titles of the claims it is easily confused with. */
  let contrastTitles = new Map();

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

  /** Centres and scales a set of cluster boxes, titles included. */
  function frameOf(boxes) {
    const x0 = Math.min(...boxes.map((c) => c.x0)) - 90; // titles can overhang
    const x1 = Math.max(...boxes.map((c) => c.x1)) + 90;
    const y0 = Math.min(...boxes.map((c) => c.y0)) - 120; // the cluster titles
    const y1 = Math.max(...boxes.map((c) => c.y1)) + 56;
    const top = 96; // the HUD
    const k = clamp(Math.min((W - 48) / (x1 - x0), (H - top - 32) / (y1 - y0)), K_MIN, 1.15);
    return { x: (x0 + x1) / 2, y: (y0 + y1) / 2 - top / (2 * k), k };
  }

  /**
   * Frames the whole graph. Every topic belongs on screen: the cluster titles
   * are drawn in screen space (see drawClusterTitles), so a far-out view still
   * says which topic is where instead of cropping half the map away.
   */
  function fitTarget() {
    const clusters = world?.clusters ?? [];
    if (!clusters.length) return { x: 0, y: 0, k: 1 };
    return frameOf(clusters);
  }

  function fit(instant) {
    if (!world || !world.nodes.length) return;
    const target = fitTarget();
    if (instant || reduced) Object.assign(cam, target), (camTarget = null);
    else camTarget = target;
    request();
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

  /** Centres a topic's cluster at a readable zoom; the map's own pan, nothing new. */
  function jumpTo(cluster) {
    if (!world || !cluster) return;
    camTarget = frameOf([cluster]);
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
   * separates the clusters and brings it back. `titleBox`
   * is screen space: the click hit test compares pointer coordinates.
   */
  function drawClusterTitles() {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const tfont = `600 16px ${FONT_READ}`;
    const sfont = `400 13px ${FONT_UI}`;
    const placed = [];
    const nodePts = world.nodes.map((n) => toScreen(n.x, n.y));
    const order = [...world.clusters].sort((a, b) => b.nodes.length - a.nodes.length);
    for (const c of order) {
      const sub = checksLine(c.checks);
      ctx.font = tfont;
      c.lines ??= split(ctx, c.path, 360);
      const widest = Math.max(...c.lines.map((line) => ctx.measureText(line).width));
      ctx.font = sfont;
      const totalW = Math.max(widest, sub ? ctx.measureText(sub).width : 0);
      const lineH = 20;
      const height = c.lines.length * lineH + (sub ? 20 : 0) + 4;
      const top = toScreen(c.cx, c.y0).y;
      const x = clamp(toScreen(c.cx, c.y0).x, totalW / 2 + 10, Math.max(totalW / 2 + 10, W - totalW / 2 - 10));
      const boxFor = (y0) => ({ x0: x - totalW / 2, x1: x + totalW / 2, y0, y1: y0 + height });
      const hits = (b) => placed.some((p) => p.x0 < b.x1 && b.x0 < p.x1 && p.y0 < b.y1 && b.y0 < p.y1);
      // A title may not sit on another cluster's ideas either: the map's job at
      // this zoom is to show where each topic is, not to overprint it.
      const clears = (b) => !nodePts.some((p) => p.x > b.x0 - 6 && p.x < b.x1 + 6 && p.y > b.y0 - 6 && p.y < b.y1 + 6);
      const candidates = [boxFor(top - 10 - height), boxFor(top - 10 - 2 * height - 8)].filter(
        (b) => b.y0 > 52 && b.y1 < H - 8,
      );
      const box = c === focusCluster ? (candidates[0] ?? null) : (candidates.find((b) => !hits(b) && clears(b)) ?? null);
      if (!box) {
        c.titleBox = null;
        continue;
      }
      placed.push(box);
      c.titleBox = box;
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
    }
  }

  function drawEdges(dimOf, spot) {
    for (const e of world.edges) {
      const dim = Math.min(dimOf(e.a), dimOf(e.b));
      const hot = spot && (e.a === spot || e.b === spot);
      const p = curve(e, 0);
      ctx.beginPath();
      ctx.moveTo(p.x0, p.y0);
      ctx.bezierCurveTo(p.x0, p.y1, p.x3, p.y2, p.x3, p.y3);
      ctx.strokeStyle = forced ? 'CanvasText' : hot ? C.edgeHot : C.edge;
      ctx.globalAlpha = forced ? 1 : dim;
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
  }

  function drawNodes(k, dimOf, spot) {
    const labelNodes = [];
    for (const n of world.nodes) {
      const target = n === hover || n === focusNode ? 1.16 : 1;
      n.scale += (target - n.scale) * (reduced ? 1 : 0.2);
      const r = n.r * n.scale;
      const dim = dimOf(n);
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
      if (k >= 0.55 || n === spot || selected.has(n.key)) labelNodes.push(n);
      ctx.globalAlpha = 1;
    }

    // Labels last, decluttered: the idea in focus wins, then selected ideas,
    // then the rest in graph order; a label that would overlap one already
    // drawn is dropped (the hover card still shows every title in full).
    const boxes = [];
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
      const box = {
        x0: n.x - widest / 2,
        x1: n.x + widest / 2,
        y0: n.y + r + 6,
        y1: n.y + r + 8 + n.lines.length * 16,
      };
      if (boxes.some((b) => b.x0 < box.x1 && box.x0 < b.x1 && b.y0 < box.y1 && box.y0 < b.y1)) continue;
      boxes.push(box);
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

    drawClusterTitles();
    ctx.setTransform(dpr * k, 0, 0, dpr * k, dpr * (W / 2 - cam.x * k), dpr * (H / 2 - cam.y * k));

    const spot = hover ?? focusNode;
    const near = spot ? new Set([spot, ...spot.needs, ...spot.leads]) : null;
    const dimOf = (n) => (near && !near.has(n) ? 0.28 : 1);

    drawEdges(dimOf, spot);
    drawNodes(k, dimOf, spot);
    drawEffects(t);
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
  }

  function placeCard() {
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
    if (camTarget || sparks.length || rings.length) return true;
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
        const e = 0.14;
        cam.x += (camTarget.x - cam.x) * e;
        cam.y += (camTarget.y - cam.y) * e;
        cam.k += (camTarget.k - cam.k) * e;
        if (Math.abs(camTarget.x - cam.x) < 0.5 && Math.abs(camTarget.y - cam.y) < 0.5 && Math.abs(camTarget.k - cam.k) < 0.002) camTarget = null;
      }
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
      const [a, b] = [...pointers.values()];
      pinch = { d: Math.hypot(a.x - b.x, a.y - b.y), k: cam.k };
      if (drag?.node) drag.node.fx = drag.node.fy = null;
      drag = null;
      return;
    }
    const w = toWorld(s.x, s.y);
    const n = hit(world, w.x, w.y);
    camTarget = null;
    if (n) {
      n.fx = n.x;
      n.fy = n.y;
      drag = { node: n, sx: s.x, sy: s.y, moved: false, vx: 0, vy: 0, last: s, lastT: e.timeStamp };
      reheat(world, 0.3);
    } else {
      drag = { pan: true, sx: s.x, sy: s.y, cx: cam.x, cy: cam.y, moved: false };
    }
    request();
  });

  canvas.addEventListener('pointermove', (e) => {
    if (!world) return;
    const s = local(e);
    if (pointers.has(e.pointerId)) pointers.set(e.pointerId, s);
    if (pinch && pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      cam.k = clamp((pinch.k * Math.hypot(a.x - b.x, a.y - b.y)) / pinch.d, K_MIN, K_MAX);
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
        reheat(world, 0.25);
      }
      request();
      return;
    }
    const w = toWorld(s.x, s.y);
    const n = hit(world, w.x, w.y);
    if (n) {
      cancelHide();
      if (n !== hover) {
        hover = n;
        closeFlyout();
      }
      canvas.style.cursor = 'pointer';
      request();
    } else {
      canvas.style.cursor = 'grab';
      if (hover) hideSoon();
    }
  });

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
      if (d.moved) {
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
      const c = world.clusters.find((cl) => cl.titleBox && s.x >= cl.titleBox.x0 && s.x <= cl.titleBox.x1 && s.y >= cl.titleBox.y0 && s.y <= cl.titleBox.y1);
      if (c) toggleCluster(c);
    }
    request();
  }
  canvas.addEventListener('pointerup', endPointer);
  canvas.addEventListener('pointercancel', endPointer);
  canvas.addEventListener('pointerleave', () => {
    if (!drag && hover) hideSoon();
  });

  canvas.addEventListener(
    'wheel',
    (e) => {
      if (!world) return;
      e.preventDefault();
      const s = local(e);
      const before = toWorld(s.x, s.y);
      cam.k = clamp(cam.k * Math.exp(-e.deltaY * 0.0015), K_MIN, K_MAX);
      const after = toWorld(s.x, s.y);
      cam.x += before.x - after.x;
      cam.y += before.y - after.y;
      camTarget = null;
      request();
    },
    { passive: false },
  );
  canvas.addEventListener('dblclick', (e) => {
    const s = local(e);
    const w = toWorld(s.x, s.y);
    if (world && !hit(world, w.x, w.y)) fit(false);
  });
  fitButton.addEventListener('click', () => fit(false));

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

  /** Renders the map; the selection keeps the ideas that still exist. */
  function render(graph, reviewIn = null, learningIn = null) {
    reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
    forced = matchMedia('(forced-colors: active)').matches;
    if (forced) reduced = true;
    learn = learningIn;
    renderLearning();
    cancelHide();
    entries = new Map();
    sparks = [];
    rings = [];
    hover = focusNode = focusCluster = null;
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
      status.replaceChildren(h('p', 'map-status-text', EMPTY));
      selected.clear();
      changed();
      request();
      return;
    }
    status.replaceChildren();
    fitButton.hidden = false;
    world = createWorld(clusterGroups(topics, nodes, graph?.topicChecks ?? {}), links, { burst: !reduced });
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
    changed();
    request();
  }

  function loading() {
    world = null;
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
    status.replaceChildren(h('p', 'map-status-text', 'Loading…'));
    request();
  }

  function failed(text, retry) {
    world = null;
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
