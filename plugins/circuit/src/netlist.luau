--!strict
--[[
  Connectivity: which points form one node.

  Wires and closed switches merge their two points, and any pin that lies on
  a wire between its ends is joined to it (a T-junction). Every ground point
  is node 0. A wire whose current is read (`meters`) keeps its ends apart: the
  solver makes it a 0 V source.

  Nodes are named by a `node` or `terminal` part sitting on them; the rest get
  letters (A, B, …) when the figure labels nodes, and otherwise a description
  of the parts touching them ("node R1–R2").
]]
local model = require("./model")

export type Netlist = {
  points: { string }, -- every point a part touches, first seen first
  nodeOf: { [string]: number }, -- point → node number; 0 is ground
  names: { string }, -- node number → display name
  groundName: string,
  count: number, -- nodes other than ground
  touching: { { string } }, -- node number → sorted names of the parts on it
  rep: { [number]: string }, -- node number (0 too) → the point its label sits at
  labelled: { [number]: boolean }, -- nodes a `node`/`terminal` part names (and draws)
  open: { [number]: boolean }, -- nodes with a `terminal`: an end left open on purpose
  described: { [number]: boolean }, -- nodes whose name describes them ("node R1–R2")
  ends: { [string]: number }, -- point → conductor ends meeting there (3+ is a junction)
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

local function gcd(a: number, b: number): number
  while b ~= 0 do
    a, b = b, a % b
  end
  return a
end

-- The grid points strictly between two cells, on the straight line joining them.
function netlist.between(a: model.Cell, b: model.Cell): { string }
  local dx, dy = b.x - a.x, b.y - a.y
  local steps = gcd(math.abs(dx), math.abs(dy))
  local keys = {}
  for k = 1, steps - 1 do
    table.insert(keys, (a.x + dx / steps * k) .. "," .. (a.y + dy / steps * k))
  end
  return keys
end

-- The node at `point`: 0 for ground, nil when no part touches it.
function netlist.at(net: Netlist, point: model.Point?): number?
  local key = model.key(point)
  return if key then net.nodeOf[key] else nil
end

-- The name of node `node`, ground included.
function netlist.name(net: Netlist, node: number): string
  return if node == 0 then net.groundName else net.names[node]
end

local function isLabel(kind: string): boolean
  return kind == "probe" or kind == "node" or kind == "terminal"
end

-- Names of the parts on each node, sorted.
local function touchingParts(parts: { model.Part }, net: Netlist): { { string } }
  local sets: { [number]: { [string]: boolean } } = {}
  for _, part in parts do
    local kind = part.type
    if kind ~= "wire" and kind ~= "ground" and kind ~= "switch" and not isLabel(kind) then
      for _, pin in model.pins(part, kind == "opamp") do
        local node = netlist.at(net, pin)
        if node and node > 0 then
          local labels = sets[node] or {}
          sets[node] = labels
          labels[model.nameOf(part)] = true
        end
      end
    end
  end
  local touching: { { string } } = {}
  for node = 1, net.count do
    local labels = {}
    for label in sets[node] or {} do
      table.insert(labels, label)
    end
    table.sort(labels)
    touching[node] = labels
  end
  return touching
end

-- Fills `net.names`: labels from `node`/`terminal` parts, then letters or
-- descriptions for the rest.
local function nameNodes(parts: { model.Part }, net: Netlist, roots: { string }, letters: boolean, warnings: { string })
  local names: { string } = {}
  local taken: { [string]: boolean } = {}
  for _, part in parts do
    local label = part.label
    if part.type == "terminal" then
      local at = netlist.at(net, part.from)
      if at then
        net.open[at] = true
      end
    end
    if (part.type == "node" or part.type == "terminal") and label then
      local key = model.key(part.from)
      local node = if key then net.nodeOf[key] else nil
      if not key or not node then
        table.insert(warnings, "`" .. label .. "` is not on a point any part touches")
      elseif node == 0 then
        net.groundName = label
        net.rep[0] = key
        net.labelled[0] = true
      elseif not names[node] then
        if taken[label] then
          table.insert(warnings, "two nodes are named `" .. label .. "`")
        end
        names[node] = label
        taken[label] = true
        net.rep[node] = key
        net.labelled[node] = true
      end
    end
  end

  if letters then
    local nextLetter = 0
    for node = 1, net.count do
      if not names[node] then
        local name
        repeat
          nextLetter += 1
          name = if nextLetter <= 26 then string.char(64 + nextLetter) else "N" .. nextLetter
        until not taken[name]
        names[node] = name
        taken[name] = true
      end
    end
  else
    -- Nodes touched by the same parts (both ends of a lone R1) also get their point.
    local uses: { [string]: number } = {}
    local described = net.described
    for node, point in roots do
      if not names[node] then
        local labels = net.touching[node]
        local name = if #labels > 0 then "node " .. table.concat(labels, "–") else "node (" .. point .. ")"
        names[node] = name
        described[node] = true
        uses[name] = (uses[name] or 0) + 1
      end
    end
    for node, point in roots do
      if described[node] and uses[names[node]] > 1 then
        names[node] ..= " (" .. point .. ")"
      end
    end
  end
  net.names = names
end

function netlist.build(parts: { model.Part }, meters: { [model.Part]: boolean }, letters: boolean, warnings: { string }): Netlist
  local parent: Parents = {}
  local points: { string } = {}
  local ends: { [string]: number } = {}
  local function ensure(point: string?): string?
    if point and not parent[point] then
      parent[point] = point
      table.insert(points, point)
    end
    return point
  end
  local function meet(point: string?, count: number)
    if point then
      ends[point] = (ends[point] or 0) + count
    end
  end

  -- Points that join a wire they lie on even though no part ends there.
  local wanted: { [string]: boolean } = {}
  local function want(point: model.Point?)
    local key = model.key(point)
    if key then
      wanted[key] = true
    end
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
        meet(point, 1)
      end
    elseif isLabel(kind) then
      want(part.from)
      if kind == "probe" then
        want(part.to)
      end
    else
      local a, b = ensure(model.key(part.from)), ensure(model.key(part.to))
      meet(a, 1)
      meet(b, 1)
      if kind == "opamp" then
        meet(ensure(model.key(part.third)), 1)
      end
      if a and b and model.isShort(part) and not meters[part] then
        union(parent, a, b)
      end
      local control = part.control
      if control then
        want(control.plus)
        want(control.minus)
      end
    end
  end

  -- T-junctions: a pin on a wire joins it; a pin on another part's body does not.
  for _, part in parts do
    local from, to = model.cell(part.from), model.cell(part.to)
    if from and to and model.isTwoTerminal(part.type) then
      local a = model.key(part.from) :: string
      local joins = model.isShort(part) and not meters[part]
      for _, point in netlist.between(from, to) do
        if joins and (parent[point] or wanted[point]) then
          ensure(point)
          union(parent, point, a)
          meet(point, 2)
        elseif not joins and parent[point] then
          table.insert(
            warnings,
            "a part ends at (" .. point .. ") on the body of `" .. model.nameOf(part) .. "`; they are not joined there"
          )
        end
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
  local rep: { [number]: string } = { [0] = groundPoint }
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
      -- A node's label sits at its busiest point.
      local best = rep[node]
      if not best or (ends[point] or 0) > (ends[best] or 0) then
        rep[node] = point
      end
    end
  end

  local net: Netlist = {
    points = points,
    nodeOf = nodeOf,
    names = {},
    groundName = "ground",
    count = #roots,
    touching = {},
    rep = rep,
    labelled = {},
    open = {},
    described = {},
    ends = ends,
  }
  net.touching = touchingParts(parts, net)
  nameNodes(parts, net, roots, letters, warnings)
  return net
end

return netlist
