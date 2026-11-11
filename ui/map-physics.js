// Physics for the Map page (docs/ui-design.md §7.6). Pure: no DOM, no clock,
// no randomness that isn't seeded from ids, so the same knowledge graph always
// settles into the same picture (mental-map stability [Misue 1995]).
//
// One cluster per topic that owns ideas. The layered layout (map-layout.js)
// gives every idea a home inside its topic: prerequisites above, dependents
// below. Clusters nest the way the topic tree does: a topic's block holds its
// own ideas and its sub-topics' blocks, and every root topic (Mathematics,
// History, …) is a separate island with wide water around it, placed next to
// the roots it shares prerequisites with. The simulation adds life around
// that home — springs along the global prerequisite edges (cross-topic ones
// included, but weak), charge between ideas, collision, and a pull toward the
// home slot — so the map explodes into place on open, can be dragged and
// flung, and always relaxes back into a readable layered shape.

import { layoutPlan } from './map-layout.js';

/** World units per layered-layout slot. */
const STEP_X = 168;
const STEP_Y = 150;
/** Gaps between the blocks inside a sub-topic (deeper than a root's children). */
const CLUSTER_GAP = 260;
const SHELF_GAP = 220;
/** Gaps between a root topic's own blocks: a little more air between fields of one subject. */
const SUB_GAP = 360;
const SUB_SHELF_GAP = 300;
/** Gaps between root topics: unrelated subjects are separate islands. */
const ROOT_GAP = 1100;
/** …and roots that share a prerequisite sit closer. */
const RELATED_GAP = 640;
/** Room above a topic block that has no ideas of its own, for its heading. */
const HEADER = 160;
/** A block's shelves wrap to keep it near this width : height. */
const ASPECT = 1.6;
/** Spring strength across clusters: a prerequisite in another topic leans, it doesn't drag. */
const SPRING_SAME_ROOT = 0.1;
const SPRING_OTHER_ROOT = 0.02;

const VELOCITY_DECAY = 0.55; // damping: a nudge dies out instead of swinging
const ALPHA_DECAY = 0.035; // cooling: rest in ~3 s, quality unchanged at 0.035 (measured)
const ALPHA_MIN = 0.002;
const CHARGE = 2000;
const LINK_DISTANCE = 130;
const HOME_X = 0.16; // the layered slot holds firmly (owner: "stiff, not elastic")
const HOME_Y = 0.22;
const COLLIDE_PAD = 34; // room between two discs
const LABEL_W = 158; // two labels in one row need this much centre distance
const LABEL_H = 56;
const MAX_SPEED = 48; // units per tick; keeps the opening burst on screen

/** Deterministic 32-bit hash of a string → [0, 1). */
function hash01(text) {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return ((h >>> 0) % 100000) / 100000;
}

/**
 * Packs boxes `{ w, h }` into shelves, left to right, each shelf centred and
 * its boxes top-aligned. A shelf wraps once it would grow wider than a block
 * of ASPECT holding every box (never narrower than the widest box).
 * `gapOf(prev, next)` is the horizontal gap between two neighbours.
 *
 * @returns {{ w: number, h: number, at: { x: number, y: number }[] }}
 */
function pack(items, gapX, gapY, gapOf = () => gapX) {
  const widest = Math.max(0, ...items.map((it) => it.w));
  const area = items.reduce((sum, it) => sum + (it.w + gapX) * (it.h + gapY), 0);
  const limit = Math.max(widest, Math.sqrt(area * ASPECT));
  const rows = [];
  let row = null;
  items.forEach((it, i) => {
    const gap = row ? gapOf(items[i - 1], it) : 0;
    if (!row || row.w + gap + it.w > limit) {
      row = { start: i, xs: [0], w: it.w, h: it.h };
      rows.push(row);
      return;
    }
    row.xs.push(row.w + gap);
    row.w += gap + it.w;
    row.h = Math.max(row.h, it.h);
  });
  const w = Math.max(0, ...rows.map((r) => r.w));
  const at = [];
  let y = 0;
  rows.forEach((r, ri) => {
    if (ri) y += gapY;
    const x0 = (w - r.w) / 2;
    r.xs.forEach((x, j) => (at[r.start + j] = { x: x0 + x, y }));
    y += r.h;
  });
  return { w, h: y, at };
}

/**
 * The topic tree the clusters hang from: every group's topic plus its
 * ancestors, in first-seen order (the caller's tree order). A topic not in
 * `topics` is a root of its own; a parent loop is cut where it repeats.
 */
function topicTree(groups, topics) {
  const source = new Map((topics ?? []).map((t) => [t.id, t]));
  const made = new Map();
  const roots = [];
  const chainOf = (id) => {
    const chain = [];
    const seen = new Set();
    for (let cur = id; cur != null && !seen.has(cur); cur = source.get(cur)?.parent ?? null) {
      seen.add(cur);
      chain.unshift(cur);
    }
    return chain;
  };
  for (const group of groups) {
    let parent = null;
    for (const id of chainOf(group.id)) {
      let t = made.get(id);
      if (!t) {
        const title = source.get(id)?.title || (id ? String(id) : 'Unfiled');
        t = { id, title, parent, children: [], own: null, cluster: null, nodes: [], subtopics: 0 };
        made.set(id, t);
        if (parent) parent.children.push(t);
        else roots.push(t);
      }
      parent = t;
    }
  }
  const list = [];
  const walk = (t, depth) => {
    t.depth = depth;
    t.chain = t.parent ? [...t.parent.chain, t] : [t];
    t.root = t.chain[0];
    t.path = t.chain.map((x) => x.title).join(' › ');
    list.push(t);
    for (const c of t.children) walk(c, depth + 1);
  };
  for (const r of roots) walk(r, 0);
  return { roots, list, made };
}

/** Sizes a topic's block from its own cluster and its sub-topics' blocks. */
function measure(t) {
  for (const c of t.children) measure(c);
  const items = [];
  if (t.own) items.push({ own: t.own, w: t.own.w, h: t.own.h });
  for (const c of t.children) items.push({ topic: c, w: c.w, h: c.h });
  const shallow = t.depth === 0;
  t.items = items;
  t.packed = pack(items, shallow ? SUB_GAP : CLUSTER_GAP, shallow ? SUB_SHELF_GAP : SHELF_GAP);
  // A topic with ideas of its own is headed by its own cluster's title; one
  // that only holds sub-topics gets a band for its heading.
  t.header = t.own || !t.children.length ? 0 : HEADER;
  t.w = t.packed.w;
  t.h = t.packed.h + t.header;
}

/** Gives a topic's block its world box and every own cluster its origin. */
function place(t, x, y) {
  Object.assign(t, { x0: x, y0: y, x1: x + t.w, y1: y + t.h, cx: x + t.w / 2, cy: y + t.h / 2 });
  t.items.forEach((it, i) => {
    const at = t.packed.at[i];
    if (it.topic) place(it.topic, x + at.x, y + t.header + at.y);
    else Object.assign(it.own, { x: x + at.x, y: y + t.header + at.y });
  });
}

/**
 * Root order: the caller's first root, then repeatedly the root sharing the
 * most prerequisites with those already placed (ties keep the caller's
 * order), so related subjects land next to each other.
 */
function rootOrder(roots, weight) {
  const left = [...roots];
  const order = [left.shift()];
  while (left.length) {
    let best = 0;
    let bestW = -1;
    left.forEach((r, i) => {
      const w = order.reduce((sum, p) => sum + weight(p, r), 0);
      if (w > bestW) {
        best = i;
        bestW = w;
      }
    });
    order.push(left.splice(best, 1)[0]);
  }
  return order;
}

/**
 * Builds the world for a knowledge graph: one cluster per topic group, nested
 * in topic blocks, root topics as separate islands, and every prerequisite
 * edge in the graph.
 *
 * @param {{ id: string, path: string, nodes: { id: string, title: string, statement?: string, kind?: string }[] }[]} groups
 *   topics that own ideas, in display order; `path` is the full topic path (cluster label)
 * @param {{ from: string, to: string }[]} links  prerequisite (`requires`) edges only, by node id
 * @param {{ burst?: boolean, topics?: { id: string, title?: string, parent?: string | null }[] }} [opts]
 *   burst: start every idea at its cluster's centre; topics: the topic tree
 *   (without it every group is a root of its own)
 * @returns world: `nodes`, `edges`, `clusters` (groups order), `topics` (the
 *   tree in pre-order: `{ id, title, path, parent, children, chain, root,
 *   cluster, nodes, subtopics, depth, x0, y0, x1, y1, cx, cy, w, h }`),
 *   `roots`, `alpha`
 */
export function createWorld(groups, links, { burst = true, topics = null } = {}) {
  const nodes = [];
  const edges = [];
  const clusters = [];
  const byId = new Map();
  const prerequisites = new Map();
  for (const { from, to } of links) {
    if (from === to) continue;
    (prerequisites.get(to) ?? prerequisites.set(to, []).get(to)).push(from);
  }

  // Size every cluster from the layering of its own ideas. Only in-cluster
  // links shape the layering; cross-topic ones are drawn by the simulation.
  const tree = topicTree(groups, topics);
  const sized = groups.map((group) => {
    const layout = layoutPlan(group.nodes.map((n) => ({ id: n.id, requires: prerequisites.get(n.id) ?? [] })));
    const widest = Math.max(1, (layout.width + 12) / 156);
    const layers = Math.max(1, Math.round((layout.height + 40) / 96));
    const s = { group, layout, w: widest * STEP_X, h: layers * STEP_Y, x: 0, y: 0, topic: tree.made.get(group.id) };
    s.topic.own = s;
    return s;
  });

  // Which root each idea lives under, and how many prerequisites two roots share.
  const rootOfNode = new Map();
  for (const s of sized) for (const n of s.group.nodes) rootOfNode.set(n.id, s.topic.root);
  const shared = new Map();
  const pairKey = (a, b) => (a.id < b.id ? `${a.id}\u0000${b.id}` : `${b.id}\u0000${a.id}`);
  for (const { from, to } of links) {
    const a = rootOfNode.get(from);
    const b = rootOfNode.get(to);
    if (a && b && a !== b) shared.set(pairKey(a, b), (shared.get(pairKey(a, b)) ?? 0) + 1);
  }
  const weight = (a, b) => (a === b ? 0 : (shared.get(pairKey(a, b)) ?? 0));

  // Blocks inside out, then the roots as islands: related neighbours get the
  // narrower strait, the whole map is centred on (0, 0).
  for (const r of tree.roots) measure(r);
  const roots = tree.roots.length ? rootOrder(tree.roots, weight) : [];
  const sea = pack(roots, ROOT_GAP, ROOT_GAP, (a, b) => (weight(a, b) ? RELATED_GAP : ROOT_GAP));
  roots.forEach((r, i) => place(r, sea.at[i].x - sea.w / 2, sea.at[i].y - sea.h / 2));

  for (const s of sized) {
    const { x, y } = s;
    const t = s.topic;
    const cluster = {
      id: s.group.id,
      path: s.group.path,
      checks: s.group.checks ?? null,
      topic: t,
      root: t.root,
      cx: x + s.w / 2,
      cy: y + s.h / 2,
      w: s.w,
      h: s.h,
      x0: x,
      y0: y,
      x1: x + s.w,
      y1: y + s.h,
      nodes: [],
    };
    t.cluster = cluster;
    clusters.push(cluster);
    for (const id of s.layout.order) {
      const src = s.group.nodes.find((n) => n.id === id);
      if (!src) continue;
      const at = s.layout.pos.get(id);
      // map-layout's boxes are 144×56 on a 156/96 grid; re-space onto ours.
      const homeX = x + (at.x / 156) * STEP_X + STEP_X / 2;
      const homeY = y + at.layer * STEP_Y + STEP_Y / 2;
      const seed = hash01(id);
      const angle = seed * Math.PI * 2;
      const node = {
        key: id,
        id,
        title: src.title ?? id,
        statement: src.statement ?? '',
        kind: src.kind ?? '',
        cluster,
        homeX,
        homeY,
        x: burst ? cluster.cx + Math.cos(angle) * 40 : homeX,
        y: burst ? cluster.cy + Math.sin(angle) * 40 : homeY,
        vx: burst ? Math.cos(angle) * (10 + seed * 8) : 0,
        vy: burst ? Math.sin(angle) * (10 + seed * 8) : 0,
        scale: 1,
        fx: null,
        fy: null,
        r: 12,
        degree: 0,
        needs: [],
        leads: [],
        seed,
      };
      byId.set(id, node);
      nodes.push(node);
      cluster.nodes.push(node);
      for (const up of t.chain) up.nodes.push(node);
    }
  }
  for (const t of tree.list) t.subtopics = tree.list.filter((d) => d !== t && d.cluster && d.chain.includes(t)).length;

  // The graph's own edges, cross-topic and multi-parent ones included. A
  // spring across clusters is weak, so a prerequisite in another topic (or
  // another subject) leans on its dependant instead of dragging it home.
  for (const { from, to } of links) {
    const a = byId.get(from);
    const b = byId.get(to);
    if (!a || !b || a === b) continue;
    const spring = a.cluster === b.cluster ? 1 : a.cluster.root === b.cluster.root ? SPRING_SAME_ROOT : SPRING_OTHER_ROOT;
    edges.push({ a, b, spring });
    a.leads.push(b);
    b.needs.push(a);
    a.degree++;
    b.degree++;
  }
  // Ideas that much else depends on are drawn bigger: they carry the graph.
  for (const n of nodes) n.r = 12 + 3 * Math.min(4, n.leads.length);

  return { nodes, edges, clusters, topics: tree.list, roots, alpha: burst ? 1 : 0 };
}

/** Wakes the simulation (after a drag, a resize, a new world). */
export function reheat(world, alpha = 0.35) {
  world.alpha = Math.max(world.alpha, alpha);
}

/** True while the simulation still moves things. */
export const active = (world) => world.alpha > ALPHA_MIN || world.nodes.some((n) => n.fx !== null);

/** One integration step (d3-force style: forces scale with alpha). */
export function tick(world) {
  const { nodes, edges } = world;
  const alpha = world.alpha;
  if (alpha <= ALPHA_MIN && !nodes.some((n) => n.fx !== null)) return;

  // Charge: every pair repels; ideas in other topics only weakly.
  for (let i = 0; i < nodes.length; i++) {
    const a = nodes[i];
    for (let j = i + 1; j < nodes.length; j++) {
      const b = nodes[j];
      let dx = b.x - a.x;
      let dy = b.y - a.y;
      let d2 = dx * dx + dy * dy;
      if (d2 < 1) {
        dx = (a.seed - b.seed) || 0.5;
        dy = 0.5;
        d2 = dx * dx + dy * dy;
      }
      const same = a.cluster === b.cluster;
      if (!same && d2 > 400 * 400) continue;
      const f = ((same ? CHARGE : CHARGE * 0.3) * alpha) / d2;
      const d = Math.sqrt(d2);
      const fx = (dx / d) * f;
      const fy = (dy / d) * f;
      a.vx -= fx;
      a.vy -= fy;
      b.vx += fx;
      b.vy += fy;
      // Collision: never let two discs overlap, nor two labels side by side
      // (a label is ~150 units wide and sits under its disc).
      const min = a.r + b.r + COLLIDE_PAD;
      if (d < min) {
        const push = ((min - d) / d) * 0.5;
        a.vx -= dx * push * 0.5;
        a.vy -= dy * push * 0.5;
        b.vx += dx * push * 0.5;
        b.vy += dy * push * 0.5;
      }
      if (same && Math.abs(dy) < LABEL_H && Math.abs(dx) < LABEL_W) {
        const side = dx === 0 ? (a.seed < b.seed ? 1 : -1) : Math.sign(dx);
        const push = (LABEL_W - Math.abs(dx)) * 0.12;
        a.vx -= side * push;
        b.vx += side * push;
      }
    }
  }

  // Springs along prerequisite edges, and a prerequisite stays above. The
  // springs are weak across topics (`spring`, set in createWorld) so the
  // blocks keep their shape; the "stays above" rule holds inside a cluster
  // only, where the layered homes agree with it — across clusters it would
  // fight the home pull and drag ideas out of their topic.
  for (const { a, b, spring = 1 } of edges) {
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const d = Math.sqrt(dx * dx + dy * dy) || 1;
    const k = (spring / Math.min(a.degree, b.degree)) * alpha;
    const pull = ((d - LINK_DISTANCE) / d) * k;
    b.vx -= dx * pull * 0.5;
    b.vy -= dy * pull * 0.5;
    a.vx += dx * pull * 0.5;
    a.vy += dy * pull * 0.5;
    if (a.cluster !== b.cluster) continue;
    // Hard rule, not a force: whatever needs an idea sits below it.
    const gap = b.y - a.y - (a.r + b.r + 96);
    if (gap < 0) {
      if (a.fx === null) a.y += gap * 0.3;
      if (b.fx === null) b.y -= gap * 0.3;
    }
  }

  // Home: the layered slot, firmly in y (keeps "needs above"), loosely in x.
  for (const n of nodes) {
    n.vx += (n.homeX - n.x) * HOME_X * alpha;
    n.vy += (n.homeY - n.y) * HOME_Y * alpha;
  }

  for (const n of nodes) {
    if (n.fx !== null) {
      n.vx = n.fx - n.x;
      n.vy = n.fy - n.y;
      n.x = n.fx;
      n.y = n.fy;
      continue;
    }
    n.vx = Math.max(-MAX_SPEED, Math.min(MAX_SPEED, n.vx * (1 - VELOCITY_DECAY)));
    n.vy = Math.max(-MAX_SPEED, Math.min(MAX_SPEED, n.vy * (1 - VELOCITY_DECAY)));
    n.x += n.vx;
    n.y += n.vy;
  }
  world.alpha += (0 - world.alpha) * ALPHA_DECAY;
}

/** Runs the simulation to rest without drawing (reduced motion, tests). */
export function settle(world, maxTicks = 600) {
  world.alpha = Math.max(world.alpha, 1);
  for (let i = 0; i < maxTicks && world.alpha > ALPHA_MIN; i++) tick(world);
  for (const n of world.nodes) {
    n.vx = 0;
    n.vy = 0;
  }
  world.alpha = 0;
}

/** Topmost node under a world point that `accept` lets through, or null. */
export function hit(world, x, y, slop = 6, accept = null) {
  let best = null;
  let bestD = Infinity;
  for (const n of world.nodes) {
    if (accept && !accept(n)) continue;
    const d = Math.hypot(n.x - x, n.y - y);
    if (d <= n.r + slop && d < bestD) {
      best = n;
      bestD = d;
    }
  }
  return best;
}
