// The plan, drawn: a layered graph of knowledge, not a list of steps.
//
// The plan's nodes ARE knowledge propositions (`graphNode` on each node) and
// its edges ARE prerequisite chains (`requires`), so this diagram shows the
// same structure the Map page shows — the plan is a window on the knowledge
// graph, filtered to what this session intends to cover. Sections are
// knowledge topics, and their order is the dependency order the chains imply.
//
// Two levels, one layout:
//   'topics' — one box per section, an edge wherever a proposition in A
//              needs one in B. Readable at plan-card width.
//   'ideas'  — every proposition, tinted by its section. Dense: the plan
//              sheet renders it at natural size inside a scroller.
//
// Layout is the classic layered pass: longest-path layers, then a barycenter
// sweep to reduce crossings, then pack each layer. Deterministic (no
// randomness, no measurement) so the same plan always draws the same way —
// boxes are sized from a per-character width estimate, the same trick the
// Map's text measure uses.
//
// A plan split into tracks (parallel branch sessions that merge) draws one
// lane per track: the root tracks side by side, each merge track below the
// lanes it joins, with a join connector from every input lane. Each lane is
// laid out with the same layered pass on its own sections.

const SVG_NS = 'http://www.w3.org/2000/svg';

const LEVELS = {
  topics: { font: 12.5, lineH: 15, padX: 14, padY: 12, minW: 108, gapX: 30, gapY: 56, wrap: 20, maxW: 1400 },
  ideas: { font: 11, lineH: 13, padX: 10, padY: 8, minW: 84, gapX: 16, gapY: 34, wrap: 18, maxW: 1000 },
};

const NARROW = new Set(" ijltfr.,:;'!|()[]-\\");
const WIDE = new Set('mwMW@');
const CAP_DIGIT = /[A-Z0-9]/;

// The "⋯" of a topic box (ui-design §7.7.1): a 20 px hit area in the box's own
// top-right corner, plus the badge text that carries the section's state.
const ACTION = 20;
const ACTION_INSET = 3;
const BADGE_SCALE = 0.86; // × the level's font, for the state words

/** Rendered width estimate for one line at `font` px. */
function lineWidth(line, font) {
  let w = 0;
  for (const ch of line) {
    if (NARROW.has(ch)) w += font * 0.28;
    else if (WIDE.has(ch)) w += font * 0.82;
    else if (CAP_DIGIT.test(ch)) w += font * 0.55;
    else w += font * 0.49;
  }
  return w;
}

/** Greedy word wrap. A word too long for one line keeps its own line (its
 *  width sizes the box) instead of being cut mid-word. */
function wrapTitle(title, maxChars) {
  const words = String(title ?? '').trim().split(/\s+/).filter(Boolean);
  const lines = [];
  let line = '';
  for (const word of words) {
    if (!line) {
      line = word;
    } else if ((line + ' ' + word).length <= maxChars) {
      line += ' ' + word;
    } else {
      lines.push(line);
      line = word;
    }
  }
  if (line) lines.push(line);
  return lines.length ? lines : [''];
}

/** Badges pack by the badge, not by the word: "aim: Analyze" is one label and
 *  never breaks across lines. */
function wrapBadges(badges, maxChars) {
  const lines = [];
  let line = '';
  for (const badge of badges) {
    if (!line) line = badge;
    else if ((line + ' · ' + badge).length <= maxChars) line += ' · ' + badge;
    else {
      lines.push(line);
      line = badge;
    }
  }
  if (line) lines.push(line);
  return lines;
}

/**
 * Longest-path layers + barycenter ordering. Returns `layers: [[id…]…]`.
 * A cycle (which `plan_write` refuses) degrades to the authoring order.
 */
function layerize(ids, edges) {
  const index = new Map(ids.map((id, i) => [id, i]));
  const preds = new Map(ids.map((id) => [id, []]));
  const succs = new Map(ids.map((id) => [id, []]));
  for (const e of edges) {
    if (!index.has(e.from) || !index.has(e.to) || e.from === e.to) continue;
    preds.get(e.to).push(e.from);
    succs.get(e.from).push(e.to);
  }
  const depth = new Map();
  const visit = (id, seen) => {
    if (depth.has(id)) return depth.get(id);
    if (seen.has(id)) return 0;
    seen.add(id);
    let d = 0;
    for (const p of preds.get(id)) d = Math.max(d, visit(p, seen) + 1);
    seen.delete(id);
    depth.set(id, d);
    return d;
  };
  for (const id of ids) visit(id, new Set());

  const maxDepth = Math.max(0, ...depth.values());
  const layers = Array.from({ length: maxDepth + 1 }, () => []);
  for (const id of ids) layers[depth.get(id)].push(id);

  // Barycenter sweeps: order each layer by the mean position of its
  // predecessors, then of its successors, keeping ties in authoring order.
  const pos = new Map();
  const assign = () => {
    for (const layer of layers) layer.forEach((id, i) => pos.set(id, i));
  };
  assign();
  const sweep = (neighbours) => {
    for (const layer of layers) {
      const key = new Map(
        layer.map((id, i) => {
          const ns = neighbours.get(id);
          const mean = ns.length ? ns.reduce((a, n) => a + (pos.get(n) ?? 0), 0) / ns.length : i;
          return [id, mean === undefined ? i : mean];
        }),
      );
      layer.sort((a, b) => key.get(a) - key.get(b));
      layer.forEach((id, i) => pos.set(id, i));
    }
  };
  for (let pass = 0; pass < 2; pass++) {
    sweep(preds);
    sweep(succs);
  }
  return layers;
}

/**
 * Draws the plan. `level` is 'topics' or 'ideas'; `statusOf(nodeId)` returns
 * 'done' | 'current' | 'open' for a plan node (only the ideas level has
 * propositions); `sectionStatus(id)` the same for a section.
 *
 * `annotate(id)` — topics level only — adds the section's state to its box:
 * `{ badges?: string[], known?: boolean, action?: { label, controls, expanded } }`.
 * Badges are words drawn under the box's title ("Current", "aim: use", "Hunt
 * on"); `known` greys the box. `action` draws a "⋯" control *inside* the box's
 * top-right corner as a focusable `role="button"` group carrying
 * `aria-label`/`aria-expanded`/`aria-controls` and `data-section-id`; the
 * caller wires click and keys by querying `.plan-node-action`.
 *
 * With `plan.tracks`, sections are drawn in lanes; `track` (a branched
 * session's own track id) emphasises that lane and dims the others.
 *
 * Returns `{ svg, width, height }` — the caller decides the frame.
 */
export function planDiagram(
  plan,
  {
    level = 'topics',
    maxW,
    scale = 1,
    sectionStatus = () => 'open',
    nodeStatus = () => 'open',
    annotate = null,
    track = null,
  } = {},
) {
  // `maxW` is the surface's own width: at or below it the diagram draws 1:1
  // (a `width: 100%` svg then never shrinks the text), above it it wraps.
  // `scale` grows the whole drawing — the card draws its boxes larger than the
  // sheet's overview does, because there the drawing is the page.
  const base = LEVELS[level] ?? LEVELS.topics;
  const spec = { ...base, maxW: maxW ?? base.maxW };
  if (scale !== 1) {
    for (const k of ['font', 'lineH', 'padX', 'padY', 'minW', 'gapX', 'gapY']) {
      spec[k] = Math.round(base[k] * scale * 10) / 10;
    }
  }
  const actionSize = Math.round(ACTION * scale);
  const sections = plan?.sections ?? [];
  const planNodes = plan?.nodes ?? [];
  const nodesById = new Map(planNodes.map((n) => [n.id, n]));

  // Build the drawn node set and its edges for this level.
  let items;
  let edges;
  if (level === 'ideas') {
    items = planNodes.map((n) => {
      const section = sectionOf(plan, n.id);
      return {
        id: n.id,
        title: n.title,
        kind: n.kind,
        section,
        status: nodeStatus(n.id),
        // Topic-level progress only: a section's evidence says nothing about a
        // single claim, so this shades the box without claiming a check.
        topic: section ? sectionStatus(section) : 'open',
      };
    });
    const ids = new Set(items.map((n) => n.id));
    edges = planNodes.flatMap((n) => (n.requires ?? []).filter((r) => ids.has(r)).map((r) => ({ from: r, to: n.id })));
  } else {
    const counts = new Map(sections.map((s) => [s.id, 0]));
    for (const n of planNodes) {
      const s = sectionOf(plan, n.id);
      if (s && counts.has(s)) counts.set(s, counts.get(s) + 1);
    }
    items = sections.map((s) => ({
      id: s.id,
      title: s.title ?? s.id,
      kind: s.mode,
      section: s.id,
      status: sectionStatus(s.id),
      // A revision section re-checks other sections' ideas: it owns none of
      // them (sectionOf), so its count is the ideas it revisits.
      count: s.revision ? (s.nodeIds ?? []).length : counts.get(s.id) ?? 0,
      revision: Boolean(s.revision),
    }));
    const seen = new Set();
    edges = [];
    for (const n of planNodes) {
      const to = sectionOf(plan, n.id);
      for (const r of n.requires ?? []) {
        const from = sectionOf(plan, r);
        if (!from || !to || from === to) continue;
        const key = `${from}\u0000${to}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({ from, to });
      }
    }
  }

  const ids = items.map((n) => n.id);
  const byId = new Map(items.map((n) => [n.id, n]));

  // Size every box from its wrapped text.
  const box = new Map();
  const annotations = new Map(); // id -> what the box draws beyond its title
  for (const item of items) {
    const lines = wrapTitle(item.title, spec.wrap);
    // The per-character estimate runs a few px short on the system UI fonts
    // (long words like "Punctured-neighborhood" spilled under the next box);
    // 8 % slack plus the padding keeps every line inside its box.
    const textW = Math.max(1, ...lines.map((l) => lineWidth(l, spec.font))) * 1.08;
    const count = item.count ?? 0;
    const ideas = count ? `${count} idea${count === 1 ? '' : 's'}` : '';
    const meta = item.revision ? ['Revision', ideas].filter(Boolean).join(' · ') : ideas;
    // The card's boxes carry their section's state and its "⋯" (topics level:
    // one box per section, so the box is the section's own control).
    const info = level === 'topics' && annotate ? annotate(item.id) : null;
    const action = info?.action ?? null;
    const badges = (info?.badges ?? []).filter(Boolean);
    const badgeLines = badges.length ? wrapBadges(badges, spec.wrap) : [];
    const badgeFont = spec.font * BADGE_SCALE;
    const badgeH = Math.round(spec.lineH * BADGE_SCALE);
    // Reserve the corner so a centred title never runs under the "⋯".
    const corner = action ? actionSize + ACTION_INSET : 0;
    const badgeW = badgeLines.length
      ? Math.max(...badgeLines.map((l) => lineWidth(l, badgeFont))) * 1.06
      : 0;
    const width = Math.max(
      spec.minW,
      textW + 2 * spec.padX + corner,
      meta ? lineWidth(meta, spec.font * 0.9) * 1.08 + 2 * spec.padX + corner : 0,
      badgeLines.length ? badgeW + 2 * spec.padX : 0,
    );
    const height =
      lines.length * spec.lineH +
      2 * spec.padY +
      (meta ? spec.lineH : 0) +
      (badgeLines.length ? badgeLines.length * badgeH + 2 : 0);
    box.set(item.id, { lines, width, height, meta, badgeLines, badgeFont, badgeH, action });
    if (info) annotations.set(item.id, info);
  }

  // Pack each layer into rows no wider than `maxW` (a 49-node layer would
  // otherwise be one 3 000 px strip), then center every row. Positions are
  // the block's own; `w`/`h` its extent.
  const rowGap = Math.round(spec.gapY * 0.4);
  const packBlock = (layers, maxW) => {
    const rowsOf = (layer) => {
      const rows = [];
      let row = [];
      let w = 0;
      for (const id of layer) {
        const bw = box.get(id).width;
        const next = w + (row.length ? spec.gapX : 0) + bw;
        if (row.length && next > maxW) {
          rows.push({ ids: row, w });
          row = [];
          w = 0;
        }
        w += (row.length ? spec.gapX : 0) + bw;
        row.push(id);
      }
      if (row.length) rows.push({ ids: row, w });
      return rows;
    };
    const layerRows = layers.map(rowsOf);
    const w = Math.max(1, ...layerRows.flat().map((r) => r.w));
    const pos = new Map();
    let y = 0;
    for (const rows of layerRows) {
      for (const row of rows) {
        let x = (w - row.w) / 2;
        const rowH = Math.max(1, ...row.ids.map((id) => box.get(id).height));
        for (const id of row.ids) {
          const b = box.get(id);
          pos.set(id, { x, y: y + (rowH - b.height) / 2 });
          x += b.width + spec.gapX;
        }
        y += rowH + rowGap;
      }
      y += spec.gapY - rowGap;
    }
    return { pos, w, h: Math.max(1, y - spec.gapY) };
  };

  const positions = new Map();
  const tracks = planTracks(plan);
  let lanes = [];
  let totalW;
  let totalH;
  if (!tracks) {
    const block = packBlock(layerize(ids, edges), spec.maxW);
    for (const [id, p] of block.pos) positions.set(id, { ...p, ...box.get(id) });
    totalW = block.w;
    totalH = block.h;
  } else {
    ({ lanes, totalW, totalH } = layoutLanes(tracks, { items, edges, box, spec, packBlock, positions, own: track }));
  }
  // With lanes, a link between two tracks is the lanes' join connector; the
  // per-box arrows stay inside their lane (a cross-lane curve would run
  // through the merge lane's revision box).
  const drawn = lanes.length ? edges.filter((e) => byId.get(e.from)?.lane === byId.get(e.to)?.lane) : edges;

  const pad = 10;
  const svg = document.createElementNS(SVG_NS, 'svg');
  // A diagram whose boxes carry a "⋯" is a group of groups: `role="img"` would
  // collapse it to one picture and the controls inside would be unreachable.
  const carriesActions = level === 'topics' && Boolean(annotate);
  svg.setAttribute('viewBox', `${-pad} ${-pad} ${totalW + 2 * pad} ${totalH + 2 * pad}`);
  svg.setAttribute('class', `plan-diagram plan-diagram-${level}`);
  svg.style.fontSize = `${spec.font}px`;
  svg.setAttribute('role', carriesActions ? 'group' : 'img');
  const trackNote = tracks ? `, ${tracks.length} tracks` : '';
  svg.setAttribute('aria-label', `Plan structure: ${items.length} ${level === 'ideas' ? 'ideas' : 'topics'}${trackNote}, ${drawn.length} prerequisite links`);
  svg.setAttribute('preserveAspectRatio', 'xMidYMin meet');

  const defs = document.createElementNS(SVG_NS, 'defs');
  const marker = document.createElementNS(SVG_NS, 'marker');
  marker.setAttribute('id', `plan-arrow-${level}`);
  marker.setAttribute('viewBox', '0 0 10 10');
  marker.setAttribute('refX', '9');
  marker.setAttribute('refY', '5');
  marker.setAttribute('markerWidth', '7');
  marker.setAttribute('markerHeight', '7');
  marker.setAttribute('orient', 'auto-start-reverse');
  const head = document.createElementNS(SVG_NS, 'path');
  head.setAttribute('d', 'M0,0 L10,5 L0,10 z');
  head.setAttribute('class', 'plan-arrow-head');
  marker.append(head);
  defs.append(marker);
  svg.append(defs);

  if (lanes.length) svg.append(laneLayer(lanes, level));

  const edgeLayer = document.createElementNS(SVG_NS, 'g');
  edgeLayer.setAttribute('class', 'plan-edges');
  for (const e of drawn) {
    const a = positions.get(e.from);
    const b = positions.get(e.to);
    if (!a || !b) continue;
    const x1 = a.x + a.width / 2;
    const y1 = a.y + a.height;
    const x2 = b.x + b.width / 2;
    const y2 = b.y;
    const mid = (y1 + y2) / 2;
    const path = document.createElementNS(SVG_NS, 'path');
    path.setAttribute('d', `M${x1},${y1} C${x1},${mid} ${x2},${mid} ${x2},${y2 - 2}`);
    const other = byId.get(e.from)?.otherLane && byId.get(e.to)?.otherLane ? ' is-other-lane' : '';
    path.setAttribute('class', `plan-edge${statusClass(byId.get(e.to))}${other}`);
    path.setAttribute('marker-end', `url(#plan-arrow-${level})`);
    edgeLayer.append(path);
  }
  svg.append(edgeLayer);

  const nodeLayer = document.createElementNS(SVG_NS, 'g');
  nodeLayer.setAttribute('class', 'plan-nodes');
  const hasActions = [...box.values()].some((b) => b.action);
  for (const item of items) {
    const p = positions.get(item.id);
    const info = annotations.get(item.id) ?? null;
    const g = document.createElementNS(SVG_NS, 'g');
    g.setAttribute(
      'class',
      [
        'plan-node',
        statusClass(item).trim(),
        topicClass(item).trim(),
        info?.known ? 'is-known' : '',
        item.revision ? 'is-revision' : '',
        item.otherLane ? 'is-other-lane' : '',
      ]
        .filter(Boolean)
        .join(' '),
    );
    g.setAttribute('transform', `translate(${p.x}, ${p.y})`);
    g.dataset.nodeId = item.id;
    if (item.kind) g.dataset.kind = item.kind;
    g.setAttribute('tabindex', '0');
    // `role="img"` on the root would hide the boxes' "⋯" from assistive tech,
    // so a diagram that carries actions is a group of groups instead.
    g.setAttribute('role', hasActions ? 'group' : 'listitem');

    const tooltip = document.createElementNS(SVG_NS, 'title');
    const needs = (nodesById.get(item.id)?.requires ?? []).length;
    const knownNote = info?.known ? 'you said you know this' : '';
    const laneNote = item.otherLane ? 'taught in another session' : '';
    tooltip.textContent =
      level === 'ideas'
        ? [`${item.title} — ${item.kind}${needs ? ` · needs ${needs}` : ''}`, laneNote].filter(Boolean).join(' · ')
        : [
            `${item.title} — ${item.revision ? 'revision, ' : ''}${item.count ?? 0} idea${(item.count ?? 0) === 1 ? '' : 's'}, ${item.kind}`,
            knownNote,
            laneNote,
          ]
            .filter(Boolean)
            .join(' · ');
    g.append(tooltip);

    const rect = document.createElementNS(SVG_NS, 'rect');
    rect.setAttribute('width', String(p.width));
    rect.setAttribute('height', String(p.height));
    rect.setAttribute('rx', level === 'ideas' ? '4' : '2');
    g.append(rect);

    // Centre the title in what the corner control leaves free.
    const textX = (p.width - (p.action ? actionSize + ACTION_INSET : 0)) / 2;
    const textY = spec.padY + spec.font * 0.85;
    p.lines.forEach((line, i) => {
      const t = document.createElementNS(SVG_NS, 'text');
      t.setAttribute('x', String(textX));
      t.setAttribute('y', String(textY + i * spec.lineH));
      t.setAttribute('text-anchor', 'middle');
      t.textContent = line;
      g.append(t);
    });
    let below = textY;
    if (p.meta) {
      below = textY + p.lines.length * spec.lineH;
      const t = document.createElementNS(SVG_NS, 'text');
      t.setAttribute('x', String(textX));
      t.setAttribute('y', String(below));
      t.setAttribute('text-anchor', 'middle');
      t.setAttribute('class', 'plan-node-meta');
      t.textContent = p.meta;
      g.append(t);
    }
    if (p.badgeLines.length) {
      below += p.meta ? 0 : p.lines.length * spec.lineH;
      p.badgeLines.forEach((line, i) => {
        const t = document.createElementNS(SVG_NS, 'text');
        t.setAttribute('x', String(p.width / 2));
        t.setAttribute('y', String(below + p.badgeH * (i + 1) - 1));
        t.setAttribute('text-anchor', 'middle');
        t.setAttribute('class', 'plan-node-badge');
        t.setAttribute('style', `font-size:${p.badgeFont.toFixed(1)}px`);
        t.textContent = line;
        g.append(t);
      });
    }
    if (p.action) {
      const act = document.createElementNS(SVG_NS, 'g');
      act.setAttribute('class', 'plan-node-action');
      act.setAttribute('transform', `translate(${p.width - actionSize - ACTION_INSET}, ${ACTION_INSET})`);
      act.setAttribute('tabindex', '0');
      act.setAttribute('role', 'button');
      act.setAttribute('aria-label', p.action.label);
      act.setAttribute('aria-expanded', String(Boolean(p.action.expanded)));
      if (p.action.controls) act.setAttribute('aria-controls', p.action.controls);
      act.dataset.sectionId = item.id;
      const hit = document.createElementNS(SVG_NS, 'rect');
      hit.setAttribute('class', 'plan-node-action-hit');
      hit.setAttribute('width', String(actionSize));
      hit.setAttribute('height', String(actionSize));
      hit.setAttribute('rx', '3');
      const dots = document.createElementNS(SVG_NS, 'text');
      dots.setAttribute('class', 'plan-node-action-mark');
      dots.setAttribute('x', String(actionSize / 2));
      dots.setAttribute('y', String(actionSize / 2));
      dots.setAttribute('text-anchor', 'middle');
      dots.setAttribute('dominant-baseline', 'central');
      dots.textContent = '⋯';
      const t = document.createElementNS(SVG_NS, 'title');
      t.textContent = p.action.label;
      act.append(t, hit, dots);
      g.append(act);
    }
    nodeLayer.append(g);
  }
  svg.append(nodeLayer);

  return { svg, width: totalW, height: totalH };
}

/**
 * Lanes for a plan with tracks. Each track's boxes get their own layered
 * block; root tracks stand side by side and a merge track sits centred under
 * the lanes it joins (`after`), recursively — the tracks form an in-tree with
 * one final track. Fills `positions` with the boxes' absolute positions, marks
 * items in another session's lane (`otherLane`) when `own` names a lane, and
 * returns the placed lanes plus the drawing's extent.
 */
function layoutLanes(tracks, { items, edges, box, spec, packBlock, positions, own }) {
  const REST = '\u0000rest'; // sections no track lists (a malformed plan) still draw
  const itemById = new Map(items.map((item) => [item.id, item]));
  const known = new Set(tracks.map((t) => t.id));
  const trackOfSection = new Map();
  for (const t of tracks) {
    for (const sid of t.sectionIds ?? []) if (!trackOfSection.has(sid)) trackOfSection.set(sid, t.id);
  }
  const members = new Map(tracks.map((t) => [t.id, []]));
  const laneOf = (item) => trackOfSection.get(item.section) ?? REST;
  for (const item of items) {
    const id = laneOf(item);
    if (!members.has(id)) members.set(id, []);
    members.get(id).push(item.id);
  }
  const all = members.has(REST) ? [...tracks, { id: REST, title: 'Other sections', after: [] }] : tracks;
  const ownLane = own && known.has(own) ? own : null;
  for (const item of items) {
    item.lane = laneOf(item);
    item.otherLane = Boolean(ownLane) && item.lane !== ownLane;
  }

  const inputsOf = new Map(
    all.map((t) => [t.id, [...new Set(t.after ?? [])].filter((a) => known.has(a) && a !== t.id)]),
  );
  const fed = new Set([...inputsOf.values()].flat());
  const leaves = Math.max(1, all.filter((t) => !inputsOf.get(t.id).length).length);

  const pad = Math.round(spec.gapX * 0.4);
  const labelFont = spec.font * BADGE_SCALE;
  const labelH = Math.round(spec.lineH * 1.6);
  const laneGap = Math.round(spec.gapX * 0.8);
  // Parallel lanes share the width; a merge lane has the whole of it.
  const leafW = Math.max(spec.minW, (spec.maxW - laneGap * (leaves - 1)) / leaves - 2 * pad);
  const lanes = new Map();
  for (const t of all) {
    const ids = members.get(t.id) ?? [];
    const inLane = new Set(ids);
    const inner = edges.filter((e) => inLane.has(e.from) && inLane.has(e.to));
    // A revision section opens its track: it sits above the lane's other
    // boxes (a layout-only link; nothing is drawn for it).
    const revisions = ids.filter((id) => itemById.get(id).revision);
    for (const r of revisions) {
      for (const id of ids) if (!revisions.includes(id)) inner.push({ from: r, to: id });
    }
    const merge = inputsOf.get(t.id).length > 0;
    const block = ids.length
      ? packBlock(layerize(ids, inner), merge ? spec.maxW - 2 * pad : leafW)
      : { pos: new Map(), w: spec.minW, h: 1 };
    const other = Boolean(ownLane) && t.id !== ownLane;
    const label = `${t.title || t.id}${other ? ' · other session' : ''}`;
    const labelW = lineWidth(label, labelFont) * 1.08 + 2 * pad;
    lanes.set(t.id, {
      id: t.id,
      label,
      labelFont,
      labelH,
      pad,
      block,
      merge,
      own: ownLane === t.id,
      other,
      inputs: inputsOf.get(t.id),
      w: Math.max(block.w + 2 * pad, labelW),
      h: labelH + block.h + pad,
    });
  }

  // A lane's column: its inputs' columns side by side, the lane under them.
  const size = new Map();
  const measure = (id, seen) => {
    if (size.has(id)) return size.get(id);
    seen.add(id);
    const lane = lanes.get(id);
    const ins = lane.inputs.filter((i) => !seen.has(i));
    let s = { w: lane.w, h: lane.h, ins, inW: 0, inH: 0 };
    if (ins.length) {
      const parts = ins.map((i) => measure(i, seen));
      const inW = parts.reduce((a, p) => a + p.w, 0) + laneGap * (parts.length - 1);
      const inH = Math.max(...parts.map((p) => p.h));
      s = { w: Math.max(inW, lane.w), h: inH + spec.gapY + lane.h, ins, inW, inH };
    }
    size.set(id, s);
    return s;
  };
  const place = (id, x, y) => {
    const s = size.get(id);
    const lane = lanes.get(id);
    lane.x = x + (s.w - lane.w) / 2;
    lane.y = s.ins.length ? y + s.inH + spec.gapY : y;
    let cx = x + (s.w - s.inW) / 2;
    for (const i of s.ins) {
      if (lanes.get(i).x !== undefined) continue;
      place(i, cx, y);
      cx += size.get(i).w + laneGap;
    }
  };
  // The final track (fed by nobody) first; anything a broken `after` left
  // unreached stands beside it.
  let cx = 0;
  let totalH = 1;
  for (const t of [...all.filter((t) => !fed.has(t.id)), ...all]) {
    if (lanes.get(t.id).x !== undefined) continue;
    const s = measure(t.id, new Set());
    place(t.id, cx, 0);
    cx += s.w + laneGap;
    totalH = Math.max(totalH, s.h);
  }

  for (const lane of lanes.values()) {
    const ox = lane.x + (lane.w - lane.block.w) / 2;
    const oy = lane.y + lane.labelH;
    for (const [id, p] of lane.block.pos) positions.set(id, { x: ox + p.x, y: oy + p.y, ...box.get(id) });
  }
  return { lanes: [...lanes.values()], totalW: Math.max(1, cx - laneGap), totalH };
}

/** The lanes' frames and titles, and a join connector into every merge lane. */
function laneLayer(lanes, level) {
  const layer = document.createElementNS(SVG_NS, 'g');
  layer.setAttribute('class', 'plan-lanes');
  const byId = new Map(lanes.map((l) => [l.id, l]));
  for (const lane of lanes) {
    const g = document.createElementNS(SVG_NS, 'g');
    g.setAttribute(
      'class',
      ['plan-lane', lane.merge ? 'is-merge' : '', lane.own ? 'is-own' : '', lane.other ? 'is-other' : ''].filter(Boolean).join(' '),
    );
    g.dataset.trackId = lane.id;
    const rect = document.createElementNS(SVG_NS, 'rect');
    rect.setAttribute('x', String(lane.x));
    rect.setAttribute('y', String(lane.y));
    rect.setAttribute('width', String(lane.w));
    rect.setAttribute('height', String(lane.h));
    rect.setAttribute('rx', '6');
    const label = document.createElementNS(SVG_NS, 'text');
    label.setAttribute('class', 'plan-lane-label');
    label.setAttribute('x', String(lane.x + lane.pad));
    label.setAttribute('y', String(lane.y + lane.labelH * 0.68));
    label.setAttribute('style', `font-size:${lane.labelFont.toFixed(1)}px`);
    label.textContent = lane.label;
    g.append(rect, label);
    layer.append(g);
  }
  for (const lane of lanes) {
    const n = lane.inputs.length;
    lane.inputs.forEach((id, k) => {
      const from = byId.get(id);
      if (!from) return;
      const x1 = from.x + from.w / 2;
      const y1 = from.y + from.h;
      const x2 = lane.x + (lane.w * (k + 1)) / (n + 1);
      const y2 = lane.y;
      const mid = (y1 + y2) / 2;
      const path = document.createElementNS(SVG_NS, 'path');
      path.setAttribute('d', `M${x1},${y1} C${x1},${mid} ${x2},${mid} ${x2},${y2 - 2}`);
      path.setAttribute('class', `plan-join${from.other && lane.other ? ' is-other' : ''}`);
      path.setAttribute('marker-end', `url(#plan-arrow-${level})`);
      layer.append(path);
    });
  }
  return layer;
}

/** 'done' / 'current' / 'open' → the className suffix the CSS keys off. */
function statusClass(item) {
  const status = item?.status ?? 'open';
  return status === 'done' ? ' is-done' : status === 'current' ? ' is-current' : '';
}

/**
 * Pan and zoom for a plan diagram shown at its natural size: drag the stage to
 * pan, wheel/pinch to zoom about the pointer, arrow keys when the stage has
 * focus, plus `zoomIn`/`zoomOut`/`fit` for the controls. Wheel zooms rather
 * than scrolls, so the diagram behaves like the map's stage (ui-design §7.6).
 *
 * `view` is the transformed wrapper; `stage` is the fixed box it moves inside.
 * The scale floor is the fit scale: fully zoomed out *is* the fit.
 */
export function attachPanZoom(stage, view, { minScale = null, maxScale = 2.6, padding = 24 } = {}) {
  let scale = 1;
  let x = 0;
  let y = 0;
  let dragging = null;
  let fitScale = 0.2;

  const art = () => ({ w: Number(view.dataset.width) || 1, h: Number(view.dataset.height) || 1 });
  const apply = () => {
    view.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px) scale(${scale.toFixed(4)})`;
  };
  const clampScale = (s) => Math.min(maxScale, Math.max(minScale ?? fitScale, s));

  function fit() {
    const { w, h } = art();
    const box = stage.getBoundingClientRect();
    if (!box.width || !box.height) return;
    fitScale = Math.min((box.width - padding * 2) / w, (box.height - padding * 2) / h);
    scale = Math.min(Math.max(fitScale, 0.05), 1.6);
    x = (box.width - w * scale) / 2;
    y = (box.height - h * scale) / 2;
    apply();
  }

  function zoomAt(factor, cx, cy) {
    const box = stage.getBoundingClientRect();
    const px = cx ?? box.width / 2;
    const py = cy ?? box.height / 2;
    const next = clampScale(scale * factor);
    if (next === scale) return;
    // Keep the point under the pointer fixed.
    x = px - (px - x) * (next / scale);
    y = py - (py - y) * (next / scale);
    scale = next;
    apply();
  }

  stage.addEventListener(
    'wheel',
    (e) => {
      e.preventDefault();
      const box = stage.getBoundingClientRect();
      zoomAt(Math.exp(-e.deltaY * 0.0015), e.clientX - box.left, e.clientY - box.top);
    },
    { passive: false },
  );

  stage.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    dragging = { id: e.pointerId, x: e.clientX, y: e.clientY, ox: x, oy: y, moved: false };
    stage.setPointerCapture(e.pointerId);
    stage.dataset.dragging = 'true';
  });
  stage.addEventListener('pointermove', (e) => {
    if (!dragging || e.pointerId !== dragging.id) return;
    const dx = e.clientX - dragging.x;
    const dy = e.clientY - dragging.y;
    if (Math.abs(dx) + Math.abs(dy) > 3) dragging.moved = true;
    x = dragging.ox + dx;
    y = dragging.oy + dy;
    apply();
  });
  const endDrag = (e) => {
    if (!dragging || (e.pointerId !== undefined && e.pointerId !== dragging.id)) return;
    stage.releasePointerCapture?.(dragging.id);
    delete stage.dataset.dragging;
    dragging = null;
  };
  stage.addEventListener('pointerup', endDrag);
  stage.addEventListener('pointercancel', endDrag);

  stage.addEventListener('keydown', (e) => {
    const step = 60;
    const moves = { ArrowLeft: [step, 0], ArrowRight: [-step, 0], ArrowUp: [0, step], ArrowDown: [0, -step] };
    if (moves[e.key]) {
      e.preventDefault();
      x += moves[e.key][0];
      y += moves[e.key][1];
      apply();
    } else if (e.key === '+' || e.key === '=') {
      e.preventDefault();
      zoomAt(1.2);
    } else if (e.key === '-') {
      e.preventDefault();
      zoomAt(1 / 1.2);
    } else if (e.key === '0') {
      e.preventDefault();
      fit();
    }
  });

  return {
    fit,
    /** Frame `rect` ({x,y,w,h} in art coords) at `scale`, centred. */
    focus: (rect, want = 0.9) => {
      const box = stage.getBoundingClientRect();
      if (!box.width || !box.height || !rect) return;
      scale = clampScale(want);
      x = box.width / 2 - (rect.x + rect.w / 2) * scale;
      y = box.height / 2 - (rect.y + rect.h / 2) * scale;
      apply();
    },
    scale: () => scale,
    zoomIn: () => zoomAt(1.2),
    zoomOut: () => zoomAt(1 / 1.2),
    /** True while the pointer is mid-drag (the overlay keeps it open). */
    dragging: () => Boolean(dragging?.moved),
  };
}

/** The section's own state, drawn on its ideas (see the sheet's caption). */
function topicClass(item) {
  if (item?.topic === 'done') return ' in-done-topic';
  if (item?.topic === 'current') return ' in-current-topic';
  return '';
}

/** The section a plan node lives in. A revision section (the start of a merge
 *  track) re-checks nodes other sections own, so a node's home is its
 *  non-revision section; a node only a revision section names lives there. */
export function sectionOf(plan, nodeId) {
  let revision = null;
  for (const s of plan?.sections ?? []) {
    if (!(s.nodeIds ?? []).includes(nodeId)) continue;
    if (!s.revision) return s.id;
    revision ??= s.id;
  }
  return revision;
}

/** The plan's tracks (parallel branches that merge), or null for an ordinary plan. */
export function planTracks(plan) {
  const tracks = plan?.tracks;
  return Array.isArray(tracks) && tracks.length ? tracks : null;
}

/** The track a section is taught in, or null. */
export function trackOf(plan, sectionId) {
  return planTracks(plan)?.find((t) => (t.sectionIds ?? []).includes(sectionId)) ?? null;
}

/**
 * The sections this session teaches, in order: in a branched session
 * (`st.branch`) only its own track's, otherwise every section. Progress,
 * "section n of m" and the head's place count these.
 */
export function ownSections(st) {
  const sections = st?.plan?.sections ?? [];
  const track = st?.branch ? planTracks(st.plan)?.find((t) => t.id === st.branch.track) : null;
  if (!track) return sections;
  const byId = new Map(sections.map((s) => [s.id, s]));
  return (track.sectionIds ?? []).map((id) => byId.get(id)).filter(Boolean);
}

/** Progress per plan node and section, from derived session state. */
export function progressOf(st) {
  const done = new Set(st?.completedSections ?? []);
  const sections = ownSections(st);
  // "Where we are" is the open section; with none open (a plan whose current
  // topic has no claims behind it, e.g. an imported one) it is the next
  // unfinished section — the same place the desk and the head point at.
  const current = st?.currentSection ?? (st?.ended ? null : sections.find((s) => !done.has(s.id))?.id ?? null);
  const sectionStatus = (id) => (done.has(id) ? 'done' : current === id ? 'current' : 'open');
  const nodeStatus = (id) => {
    const checks = st?.checks?.[id] ?? [];
    if (checks.some((c) => c.verdict === 'correct')) return 'done';
    if (st?.lastNodeId === id) return 'current';
    return 'open';
  };
  return { sectionStatus, nodeStatus };
}
