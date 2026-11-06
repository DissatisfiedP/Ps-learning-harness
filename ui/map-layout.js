// Layered layout for one plan's ideas (docs/ui-design.md §7.6): what an idea
// needs sits above it. Pure and deterministic — the same nodes always give
// the same picture — and it needs no measuring, because every box has the
// same fixed size. Steps follow the classic layered method (Sugiyama 1981):
// longest-path layering (foundations then pulled down to just above their
// first use), barycenter ordering sweeps, centred coordinates.

export const BOX = { w: 144, h: 56, gapX: 12, gapY: 40 };

const SWEEPS = 4;

/**
 * @param {{ id: string, requires?: string[] }[]} nodes in plan order
 * @returns {{
 *   order: string[],                                  layer by layer, left to right
 *   pos: Map<string, { x: number, y: number, layer: number }>,
 *   edges: { from: string, to: string }[],            prerequisite → idea, downward only
 *   width: number, height: number,
 * }}
 */
export function layoutPlan(nodes) {
  const ids = new Set(nodes.map((n) => n.id));
  // In-plan prerequisites only, without duplicates or self-references.
  const reqs = new Map(
    nodes.map((n) => [n.id, [...new Set(n.requires ?? [])].filter((r) => r !== n.id && ids.has(r))]),
  );

  // Longest path from an idea with no prerequisite. A cycle's back edge is
  // ignored (the planner should never produce one; the drawing must not hang).
  const layer = new Map();
  const visiting = new Set();
  const depth = (id) => {
    if (layer.has(id)) return layer.get(id);
    if (visiting.has(id)) return -1;
    visiting.add(id);
    let l = 0;
    for (const r of reqs.get(id)) l = Math.max(l, depth(r) + 1);
    visiting.delete(id);
    layer.set(id, l);
    return l;
  };
  for (const n of nodes) depth(n.id);

  // An idea that needs nothing sits one layer above the first idea that needs
  // it, not at the top: long edges cut across other boxes.
  const firstUse = new Map();
  for (const n of nodes) {
    for (const r of reqs.get(n.id)) {
      if (layer.get(r) < layer.get(n.id)) firstUse.set(r, Math.min(firstUse.get(r) ?? Infinity, layer.get(n.id)));
    }
  }
  for (const n of nodes) {
    if (!reqs.get(n.id).length && firstUse.has(n.id)) layer.set(n.id, firstUse.get(n.id) - 1);
  }

  const layers = [];
  for (const n of nodes) (layers[layer.get(n.id)] ??= []).push(n.id);

  const edges = [];
  const below = new Map(nodes.map((n) => [n.id, []]));
  const above = new Map(nodes.map((n) => [n.id, []]));
  for (const n of nodes) {
    for (const r of reqs.get(n.id)) {
      if (layer.get(r) >= layer.get(n.id)) continue; // a cycle's back edge
      edges.push({ from: r, to: n.id });
      above.get(n.id).push(r);
      below.get(r).push(n.id);
    }
  }

  // Centred slot of each idea in its layer, comparable across layers of
  // different widths.
  const slot = new Map();
  const place = (row) => row.forEach((id, i) => slot.set(id, i - (row.length - 1) / 2));
  layers.forEach(place);

  const sweep = (row, neighbours) => {
    const key = new Map();
    for (const id of row) {
      const ns = neighbours.get(id);
      key.set(id, ns.length ? ns.reduce((sum, x) => sum + slot.get(x), 0) / ns.length : slot.get(id));
    }
    row.sort((a, b) => key.get(a) - key.get(b) || slot.get(a) - slot.get(b));
    place(row);
  };
  for (let pass = 0; pass < SWEEPS; pass++) {
    if (pass % 2 === 0) for (let l = 1; l < layers.length; l++) sweep(layers[l], above);
    else for (let l = layers.length - 2; l >= 0; l--) sweep(layers[l], below);
  }

  const widest = Math.max(0, ...layers.map((row) => row.length));
  const stepX = BOX.w + BOX.gapX;
  const stepY = BOX.h + BOX.gapY;
  const pos = new Map();
  layers.forEach((row, l) => {
    const offset = ((widest - row.length) * stepX) / 2;
    row.forEach((id, i) => pos.set(id, { x: offset + i * stepX, y: l * stepY, layer: l }));
  });

  return {
    order: layers.flat(),
    pos,
    edges,
    width: widest ? widest * stepX - BOX.gapX : 0,
    height: layers.length ? layers.length * stepY - BOX.gapY : 0,
  };
}
