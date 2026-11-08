--!strict
-- Connectivity: which points form one node. Wires and closed switches merge
-- their points; every ground point is node 0.
local model = require("./model")

export type Netlist = {
  points: { string }, -- every point a part (other than a probe) touches, first seen first
  nodeOf: { [string]: number }, -- point → node number; 0 is ground
  names: { string }, -- node number → display name, e.g. "node R1–R2"
  count: number, -- nodes other than ground
}

type Parents = { [string]: string }

local netlist = {}

local function root(parent: Parents, point: string): string
  local r = point
  while parent[r] ~= r do
    r = parent[r]
  end
  return r
end

local function union(parent: Parents, a: string, b: string)
  local ra, rb = root(parent, a), root(parent, b)
  if ra ~= rb then
    parent[rb] = ra
  end
end

-- The node at `point`: 0 for ground, nil when no part touches it.
function netlist.at(net: Netlist, point: model.Point?): number?
  local key = model.key(point)
  return if key then net.nodeOf[key] else nil
end

-- Names each node after the parts touching it; nodes touched by the same
-- parts (both ends of a lone R1) also get their point.
local function nameNodes(parts: { model.Part }, net: Netlist, roots: { string }): { string }
  local touching: { [number]: { [string]: boolean } } = {}
  for _, part in parts do
    local kind = part.type
    if kind ~= "wire" and kind ~= "ground" and kind ~= "probe" and kind ~= "switch" then
      for _, pin in model.pins(part, kind == "opamp") do
        local node = netlist.at(net, pin)
        if node and node > 0 then
          local labels = touching[node] or {}
          touching[node] = labels
          labels[model.nameOf(part)] = true
        end
      end
    end
  end

  local names: { string } = {}
  local uses: { [string]: number } = {}
  for node, point in roots do
    local labels = {}
    for label in touching[node] or {} do
      table.insert(labels, label)
    end
    table.sort(labels)
    local name = if #labels > 0 then "node " .. table.concat(labels, "–") else "node (" .. point .. ")"
    names[node] = name
    uses[name] = (uses[name] or 0) + 1
  end
  for node, point in roots do
    if uses[names[node]] > 1 then
      names[node] ..= " (" .. point .. ")"
    end
  end
  return names
end

function netlist.build(parts: { model.Part }, warnings: { string }): Netlist
  local parent: Parents = {}
  local points: { string } = {}
  local function ensure(point: string?): string?
    if point and not parent[point] then
      parent[point] = point
      table.insert(points, point)
    end
    return point
  end

  local ground: string? = nil
  for _, part in parts do
    local kind = part.type
    if kind == "ground" then
      local point = ensure(model.key(part.from))
      if point then
        local first = ground or point
        ground = first
        union(parent, point, first)
      end
    elseif kind ~= "probe" then
      local a, b = ensure(model.key(part.from)), ensure(model.key(part.to))
      if kind == "opamp" then
        ensure(model.key(part.third))
      end
      if a and b and model.isShort(part) then
        union(parent, a, b)
      end
    end
  end
  local groundPoint = ground or points[1] or "0,0"
  if not ground then
    table.insert(warnings, "no ground: add a `ground` component")
    ensure(groundPoint)
  end
  local groundRoot = root(parent, groundPoint)

  -- Nodes are numbered in the order their first point was seen.
  local nodeOf: { [string]: number } = {}
  local numberOf: { [string]: number } = {}
  local roots: { string } = {}
  for _, point in points do
    local r = root(parent, point)
    if r == groundRoot then
      nodeOf[point] = 0
    else
      local node = numberOf[r]
      if not node then
        table.insert(roots, r)
        node = #roots
        numberOf[r] = node
      end
      nodeOf[point] = node
    end
  end

  local net: Netlist = { points = points, nodeOf = nodeOf, names = {}, count = #roots }
  net.names = nameNodes(parts, net, roots)
  return net
end

return netlist
