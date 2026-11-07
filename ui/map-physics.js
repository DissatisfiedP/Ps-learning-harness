// Physics for the Map page (docs/ui-design.md §7.6). Pure: no DOM, no clock,
// no randomness that isn't seeded from ids, so the same knowledge graph always
// settles into the same picture (mental-map stability [Misue 1995]).
//
// One cluster per topic. The layered layout (map-layout.js) gives every idea a
// home inside its topic: prerequisites above, dependents below. Topics sit on
// shelves, side by side and in the caller's topic order. The simulation adds
// life around that home — springs along the global prerequisite edges (cross-
// topic ones included), charge between ideas, collision, and a pull toward the
// home slot — so the map explodes into place on open, can be dragged and
// flung, and always relaxes back into a readable layered shape.

import { layoutPlan } from './map-layout.js';

/** World units per layered-layout slot. */
const STEP_X = 168;
const STEP_Y = 150;
/** Horizontal gap between two topics on one shelf. */
const CLUSTER_GAP = 260;
/** Vertical gap between two shelves. */
const SHELF_GAP = 220;
/** A shelf takes topics until it would grow wider than this. */
const SHELF_MAX = 2200;

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
 * Builds the world for a knowledge graph: one cluster per topic group, shelves
 * side by side, and every prerequisite edge in the graph.
 *
 * @param {{ id: string, path: string, nodes: { id: string, title: string, statement?: string, kind?: string }[] }[]} groups
 *   topics in display order; `path` is the full topic path (cluster label)
 * @param {{ from: string, to: string }[]} links  prerequisite (`requires`) edges only, by node id
 * @param {{ burst?: boolean }} [opts]  burst: start every idea at its cluster's centre
 */
export function createWorld(groups, links, { burst = true } = {}) {
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
  const sized = groups.map((group) => {
    const layout = layoutPlan(group.nodes.map((n) => ({ id: n.id, requires: prerequisites.get(n.id) ?? [] })));
    const widest = Math.max(1, (layout.width + 12) / 156);
    const layers = Math.max(1, Math.round((layout.height + 40) / 96));
    return { group, layout, w: widest * STEP_X, h: layers * STEP_Y };
  });

  // Shelves: topics in the caller's order, wrapped when a shelf is full.
  const shelves = [];
  let shelf = { items: [], w: 0, h: 0 };
  for (const s of sized) {
    if (shelf.items.length && shelf.w + CLUSTER_GAP + s.w > SHELF_MAX) {
      shelves.push(shelf);
      shelf = { items: [], w: 0, h: 0 };
    }
    shelf.w = shelf.items.length ? shelf.w + CLUSTER_GAP + s.w : s.w;
    shelf.h = Math.max(shelf.h, s.h);
    shelf.items.push(s);
  }
  if (shelf.items.length) shelves.push(shelf);

  // Centre every shelf on x = 0 and the stack of shelves on y = 0.
  const totalH = shelves.reduce((sum, row, i) => sum + row.h + (i ? SHELF_GAP : 0), 0);
  let shelfY = -totalH / 2;
  for (const row of shelves) {
    let x = -row.w / 2;
    for (const s of row.items) {
      const cluster = {
        id: s.group.id,
        path: s.group.path,
        checks: s.group.checks ?? null,
        cx: x + s.w / 2,
        cy: shelfY + s.h / 2,
        w: s.w,
        h: s.h,
        x0: x,
        y0: shelfY,
        x1: x + s.w,
        y1: shelfY + s.h,
        nodes: [],
      };
      clusters.push(cluster);
      for (const id of s.layout.order) {
        const src = s.group.nodes.find((n) => n.id === id);
        if (!src) continue;
        const at = s.layout.pos.get(id);
        // map-layout's boxes are 144×56 on a 156/96 grid; re-space onto ours.
        const homeX = x + (at.x / 156) * STEP_X + STEP_X / 2;
        const homeY = shelfY + at.layer * STEP_Y + STEP_Y / 2;
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
      }
      x += s.w + CLUSTER_GAP;
    }
    shelfY += row.h + SHELF_GAP;
  }

  // The graph's own edges, cross-topic and multi-parent ones included.
  for (const { from, to } of links) {
    const a = byId.get(from);
    const b = byId.get(to);
    if (!a || !b || a === b) continue;
    edges.push({ a, b });
    a.leads.push(b);
    b.needs.push(a);
    a.degree++;
    b.degree++;
  }
  // Ideas that much else depends on are drawn bigger: they carry the graph.
  for (const n of nodes) n.r = 12 + 3 * Math.min(4, n.leads.length);

  return { nodes, edges, clusters, alpha: burst ? 1 : 0 };
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
  // springs are weak across topics so the shelves keep their shape.
  for (const { a, b } of edges) {
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const d = Math.sqrt(dx * dx + dy * dy) || 1;
    const k = (1.0 / Math.min(a.degree, b.degree)) * alpha;
    const pull = ((d - LINK_DISTANCE) / d) * k;
    b.vx -= dx * pull * 0.5;
    b.vy -= dy * pull * 0.5;
    a.vx += dx * pull * 0.5;
    a.vy += dy * pull * 0.5;
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

/** Topmost node under a world point, or null. */
export function hit(world, x, y, slop = 6) {
  let best = null;
  let bestD = Infinity;
  for (const n of world.nodes) {
    const d = Math.hypot(n.x - x, n.y - y);
    if (d <= n.r + slop && d < bestD) {
      best = n;
      bestD = d;
    }
  }
  return best;
}
