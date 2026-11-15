--!strict
--[[
  What a solved circuit says beyond its voltages and currents, each as a line
  of the model-facing text: which parts are in series or in parallel, the
  Thevenin/Norton equivalent at two terminals, each source's share of a probe
  (superposition), and the time constant of a single-capacitor or
  single-inductor circuit.
]]
local num = require("@harness/num")
local model = require("./model")
local netlist = require("./netlist")
local phasor = require("./phasor")
local solver = require("./solver")
local format = require("./format")

type Part = model.Part

local analysis = {}

local function pointText(point: model.Point?): string
  return format.point(model.key(point) or "?")
end

-- The parts that carry a branch current between two nodes, in listed order.
local function branches(solved: solver.Solved, parts: { Part }): { Part }
  local list = {}
  for _, part in parts do
    local kind = part.type
    local counts = model.isTwoTerminal(kind) and (not model.isShort(part) or solved.meters[part])
    if counts and kind ~= "switch" and not solved.rejected[part] then
      table.insert(list, part)
    end
  end
  return list
end

-- The impedance of a passive part in this domain, or nil when it has none
-- that adds up (a DC capacitor is open, a DC inductor a short).
local function impedance(part: Part, domain: phasor.Domain): num.Number?
  local kind = part.type
  if kind == "R" then
    return tonumber(part.value)
  elseif domain.ac and (kind == "C" or kind == "L") then
    return num.div(1, phasor.admittance(kind, tonumber(part.value) :: number, domain))
  end
  return nil
end

local function names(parts: { Part }, separator: string): string
  local list = table.create(#parts)
  for i, part in parts do
    list[i] = model.nameOf(part)
  end
  return table.concat(list, separator)
end

-- "Series (same current): …" and "Parallel (same voltage): …", or "" for none.
function analysis.groups(solved: solver.Solved, parts: { Part }): { string }
  local net, domain = solved.net, solved.domain
  local list = branches(solved, parts)

  -- Parallel: the same two nodes.
  local order: { string } = {}
  local across: { [string]: { Part } } = {}
  for _, part in list do
    local a, b = netlist.at(net, part.from) or 0, netlist.at(net, part.to) or 0
    if a ~= b then
      local pair = math.min(a, b) .. ":" .. math.max(a, b)
      if not across[pair] then
        across[pair] = {}
        table.insert(order, pair)
      end
      table.insert(across[pair], part)
    end
  end
  local parallel = {}
  for _, pair in order do
    local group = across[pair]
    if #group > 1 then
      local text = names(group, " ∥ ")
      local total: num.Number? = 0
      for _, part in group do
        local z = impedance(part, domain)
        total = if z and total then num.add(total, num.div(1, z)) else nil
      end
      if total then
        text ..= " = " .. format.value(num.div(1, total), "Ω")
      end
      table.insert(parallel, text)
    end
  end

  -- Series: joined at a node nothing else touches. Op-amp pins count as touching.
  local at: { [number]: { Part } } = {}
  local busy: { [number]: boolean } = {}
  for _, part in list do
    for _, pin in model.pins(part, false) do
      local n = netlist.at(net, pin) or 0
      at[n] = at[n] or {}
      table.insert(at[n], part)
    end
  end
  for _, part in parts do
    if part.type == "opamp" and not solved.rejected[part] then
      for _, pin in model.pins(part, true) do
        busy[netlist.at(net, pin) or 0] = true
      end
    end
  end
  local chainOf: { [Part]: Part } = {}
  local function head(part: Part): Part
    local h = part
    while chainOf[h] and chainOf[h] ~= h do
      h = chainOf[h]
    end
    return h
  end
  for n, touching in at do
    if #touching == 2 and not busy[n] and touching[1] ~= touching[2] then
      chainOf[head(touching[2])] = head(touching[1])
    end
  end
  local chains: { [Part]: { Part } } = {}
  local heads: { Part } = {}
  for _, part in list do
    local h = head(part)
    if not chains[h] then
      chains[h] = {}
      table.insert(heads, h)
    end
    table.insert(chains[h], part)
  end
  local series = {}
  for _, h in heads do
    local chain = chains[h]
    if #chain > 1 then
      local text = names(chain, " – ")
      local passive, total = {}, 0 :: num.Number
      for _, part in chain do
        local z = impedance(part, domain)
        if z then
          table.insert(passive, part)
          total = num.add(total, z)
        end
      end
      if #passive > 1 then
        text ..= " (" .. names(passive, " + ") .. " = " .. format.value(total, "Ω") .. ")"
      end
      table.insert(series, text)
    end
  end

  local lines = {}
  if #series > 0 then
    table.insert(lines, "Series (same current): " .. table.concat(series, "; ") .. ".")
  end
  if #parallel > 0 then
    table.insert(lines, "Parallel (same voltage): " .. table.concat(parallel, "; ") .. ".")
  end
  return lines
end

-- `parts` with every independent source switched off except `keep`.
local function deactivated(parts: { Part }, keep: Part?): { Part }
  local copy: { Part } = {}
  for i, part in parts do
    if model.isSource(part.type) and part ~= keep then
      local off = table.clone(part)
      off.off = true
      copy[i] = off
    else
      copy[i] = part
    end
  end
  return copy
end

type Port = {
  voc: num.Number, -- V(a) − V(b) with the terminals open
  zth: num.Number?, -- nil when no finite Thevenin impedance exists
}

-- The equivalent of `parts` seen from points `a` (+) and `b` (−).
local function port(parts: { Part }, circuit: model.Circuit, a: model.Point, b: model.Point): (Port?, string?)
  local ka, kb = model.key(a), model.key(b)
  if not ka or not kb or ka == kb then
    return nil, "the equivalent needs two different points"
  end
  -- Terminals make the points join a wire they lie on.
  local open = table.clone(parts)
  table.insert(open, { type = "terminal", from = a })
  table.insert(open, { type = "terminal", from = b })

  local first = solver.run(open, circuit.analysis, false)
  local va, vb = first.solution.nodes[ka], first.solution.nodes[kb]
  if not first.ok or va == nil or vb == nil then
    return nil, "the equivalent's terminals must be on a circuit that solves with them open"
  end

  -- Thevenin impedance: independent sources off, 1 A pushed into `a`.
  local dead = deactivated(open, nil)
  table.insert(dead, { id = "test source", type = "I", from = b, to = a, value = 1, ac = { magnitude = 1, phaseDeg = 0 } })
  local second = solver.run(dead, circuit.analysis, false)
  local za, zb = second.solution.nodes[ka], second.solution.nodes[kb]
  local zth: num.Number? = if second.ok and za ~= nil and zb ~= nil then num.sub(za, zb) else nil
  return { voc = num.sub(va, vb), zth = zth }, nil
end

-- "Thevenin equivalent …" for `circuit.equivalent`, or a warning.
function analysis.equivalent(circuit: model.Circuit, byName: { [string]: Part }, warnings: { string }): string?
  local spec = circuit.equivalent
  if type(spec) ~= "table" then
    return nil
  end
  local parts = circuit.components
  local a, b, where = spec.from, spec.to, ""
  local loadName = spec.load
  if loadName then
    local load = byName[loadName]
    if not load or not model.isTwoTerminal(load.type) then
      table.insert(warnings, "the equivalent's load `" .. loadName .. "` is not a two-terminal part")
      return nil
    end
    parts = table.clone(parts)
    table.remove(parts, table.find(parts, load) :: number)
    a, b = load.from, load.to
    where = "seen by " .. model.nameOf(load) .. ", "
  end
  if not a or not b then
    table.insert(warnings, "the equivalent needs `load`, or `from` and `to`")
    return nil
  end
  local result, problem = port(parts, circuit, a :: model.Point, b :: model.Point)
  if not result then
    table.insert(warnings, problem :: string)
    return nil
  end

  local zName = if circuit.analysis.ac then "Zth" else "Rth"
  local text = "Thevenin equivalent "
    .. where
    .. "+ at "
    .. pointText(a)
    .. ", − at "
    .. pointText(b)
    .. ": Vth = "
    .. format.value(result.voc, "V")
  local zth = result.zth
  if not zth then
    return text .. "; no finite " .. zName .. " (the circuit is a current source at these terminals)."
  end
  text ..= ", " .. zName .. " = " .. format.value(zth, "Ω")
  if num.abs(zth) < 1e-9 then
    return text .. " (an ideal voltage source: no Norton form)."
  end
  text ..= "; Norton: IN = " .. format.value(num.div(result.voc, zth), "A") .. " in parallel with " .. zName
  if type(zth) == "number" and type(result.voc) == "number" and zth > 0 then
    text ..= "; a matched load (" .. format.eng(zth, "Ω") .. ") would absorb " .. format.eng(result.voc ^ 2 / (4 * zth), "W")
  end
  return text .. "."
end

-- "Time constant: …" for a DC circuit with exactly one capacitor or inductor.
function analysis.timeConstant(circuit: model.Circuit, solved: solver.Solved): string?
  if circuit.analysis.ac then
    return nil
  end
  local storage: Part? = nil
  for _, part in circuit.components do
    if (part.type == "C" or part.type == "L") and not solved.rejected[part] then
      if storage then
        return nil
      end
      storage = part
    end
  end
  if not storage or not storage.from or not storage.to then
    return nil
  end
  local rest = table.clone(circuit.components)
  table.remove(rest, table.find(rest, storage) :: number)
  local result = port(rest, circuit, storage.from, storage.to)
  local rth = if result then result.zth else nil
  if type(rth) ~= "number" or rth <= 1e-9 then
    return nil
  end
  local name, value = model.nameOf(storage), tonumber(storage.value) :: number
  local tau = if storage.type == "C" then rth * value else value / rth
  local how = if storage.type == "C" then "Rth·" .. name else name .. "/Rth"
  return "Time constant: τ = "
    .. how
    .. " = "
    .. format.eng(tau, "s")
    .. ", with Rth = "
    .. format.eng(rth, "Ω")
    .. " seen by "
    .. name
    .. " (the values above are the steady state)."
end

-- "Superposition: …": each probe as the sum of what each source gives alone.
function analysis.superposition(circuit: model.Circuit, full: model.Solution): string?
  if not circuit.superposition then
    return nil
  end
  local sources = {}
  for _, part in circuit.components do
    if model.isSource(part.type) and not part.off then
      table.insert(sources, part)
    end
  end
  if #sources < 2 then
    return "Superposition: fewer than two independent sources are on, so there is nothing to add up."
  end
  local labels = {}
  for label in full.probes do
    if full.units[label] ~= "W" then
      table.insert(labels, label)
    end
  end
  table.sort(labels)
  if #labels == 0 then
    return "Superposition: add a voltage or current probe to see each source's share (power does not superpose)."
  end

  local shares: { [string]: { string } } = {}
  for _, source in sources do
    local alone = solver.run(deactivated(circuit.components, source), circuit.analysis, false)
    for _, label in labels do
      local value = alone.solution.probes[label]
      local text = if value ~= nil then format.value(value, full.units[label]) else "?"
      shares[label] = shares[label] or {}
      table.insert(shares[label], text .. " (" .. model.nameOf(source) .. " alone)")
    end
  end
  local lines = {}
  for _, label in labels do
    table.insert(lines, label .. " = " .. format.value(full.probes[label], full.units[label]) .. " = " .. table.concat(shares[label], " + "))
  end
  return "Superposition (other sources off: V shorted, I opened): " .. table.concat(lines, "; ") .. "."
end

return analysis
