--!strict
--[[
  The node-voltage equations of a solved circuit, written the way they are
  set up by hand: one KCL equation (currents leaving = 0) per node whose
  voltage no source fixes, one per supernode (nodes tied together by a
  voltage source), and one constraint per voltage source.
]]
local model = require("./model")
local netlist = require("./netlist")
local solver = require("./solver")
local phasor = require("./phasor")
local format = require("./format")

type Part = model.Part
type Signed = { negative: boolean, text: string }

local equations = {}

local function v(net: netlist.Netlist, node: number): string
  return "V(" .. netlist.name(net, node) .. ")"
end

-- V(a) − V(b), with ground written as nothing.
local function drop(net: netlist.Netlist, a: number, b: number): string
  if a == b then
    return "0"
  elseif b == 0 then
    return v(net, a)
  elseif a == 0 then
    return "−" .. v(net, b)
  end
  return v(net, a) .. " − " .. v(net, b)
end

-- The same, bracketed when it is a difference, ready to be multiplied or divided.
local function factor(net: netlist.Netlist, a: number, b: number): string
  local text = drop(net, a, b)
  return if a ~= 0 and b ~= 0 and a ~= b then "(" .. text .. ")" else text
end

local function gainText(part: Part, spec: model.Dependent): string
  return format.eng(part.gain :: number, spec.unit)
end

-- "a + b − c" from signed terms.
local function sum(terms: { Signed }): string
  local text = ""
  for i, term in terms do
    if i == 1 then
      text = (if term.negative then "−" else "") .. term.text
    else
      text ..= (if term.negative then " − " else " + ") .. term.text
    end
  end
  return text
end

-- The lines of the nodal analysis, or none when the circuit did not solve.
function equations.nodal(solved: solver.Solved, parts: { Part }): { string }
  if not solved.ok or solved.net.count == 0 then
    return {}
  end
  local net, domain = solved.net, solved.domain
  local function at(point: model.Point?): number
    return netlist.at(net, point) or 0
  end
  local function usable(part: Part): boolean
    return not solved.rejected[part]
  end
  -- A part that fixes the voltage between its two nodes.
  local function ties(part: Part): boolean
    local kind = part.type
    local spec = model.DEPENDENT[kind]
    return kind == "V" or (kind == "L" and not domain.ac) or (spec ~= nil and spec.out == "V") or solved.meters[part] == true
  end

  -- Supernodes: nodes tied by voltage sources. An op-amp fixes its output.
  local parent: { [number]: number } = {}
  for node = 0, net.count do
    parent[node] = node
  end
  local function root(node: number): number
    while parent[node] ~= node do
      node = parent[node]
    end
    return node
  end
  local function tie(a: number, b: number)
    local ra, rb = root(a), root(b)
    if ra ~= rb then
      parent[math.max(ra, rb)] = math.min(ra, rb)
    end
  end
  for _, part in parts do
    if usable(part) then
      if part.type == "opamp" then
        tie(at(part.third), 0)
      elseif model.isTwoTerminal(part.type) and ties(part) then
        tie(at(part.from), at(part.to))
      end
    end
  end

  -- What a dependent source follows, as text; currents are defined once below.
  local defined: { [string]: boolean } = {}
  local definitions: { string } = {}
  local function currentOf(target: Part): string
    local name = model.nameOf(target)
    local symbol = "i(" .. name .. ")"
    if not defined[symbol] then
      defined[symbol] = true
      local a, b = at(target.from), at(target.to)
      local kind = target.type
      if kind == "R" then
        table.insert(definitions, symbol .. " = " .. factor(net, a, b) .. "/" .. name)
      elseif domain.ac and kind == "C" then
        table.insert(definitions, symbol .. " = jω" .. name .. "·" .. factor(net, a, b))
      elseif domain.ac and kind == "L" then
        table.insert(definitions, symbol .. " = " .. factor(net, a, b) .. "/(jω" .. name .. ")")
      elseif kind == "I" then
        table.insert(definitions, symbol .. " = " .. name)
      else
        table.insert(definitions, symbol .. " is the current through " .. name .. " from its `from` end to its `to` end")
      end
    end
    return symbol
  end
  local function controlText(part: Part, spec: model.Dependent): string
    local control = part.control :: model.Control
    local target = if control.part then solved.byName[control.part] else nil
    local gain = gainText(part, spec)
    if spec.by == "I" then
      return gain .. "·" .. (if target then currentOf(target) else "0")
    end
    local plus = if target then at(target.from) else at(control.plus)
    local minus = if target then at(target.to) else at(control.minus)
    return gain .. "·" .. factor(net, plus, minus)
  end

  -- KCL: the current each part carries out of a group of nodes.
  local groups: { [number]: { number } } = {}
  local order: { number } = {}
  for node = 1, net.count do
    local r = root(node)
    if r ~= 0 then
      if not groups[r] then
        groups[r] = {}
        table.insert(order, r)
      end
      table.insert(groups[r], node)
    end
  end

  local lines = {}
  local unknowns = table.create(net.count)
  for node = 1, net.count do
    unknowns[node] = v(net, node)
  end
  table.insert(
    lines,
    "Nodal analysis: reference node = " .. net.groundName .. " (0 V); unknowns: " .. table.concat(unknowns, ", ") .. "."
  )

  for _, r in order do
    local terms: { Signed } = {}
    for _, part in parts do
      local kind = part.type
      if usable(part) and model.isTwoTerminal(kind) and not ties(part) then
        local a, b = at(part.from), at(part.to)
        local inA, inB = root(a) == r, root(b) == r
        if inA ~= inB then
          -- `here` is the end inside the group; current leaving it is positive.
          local here, there = if inA then a else b, if inA then b else a
          local name = model.nameOf(part)
          local spec = model.DEPENDENT[kind]
          if kind == "R" then
            table.insert(terms, { negative = false, text = factor(net, here, there) .. "/" .. name })
          elseif kind == "C" and domain.ac then
            table.insert(terms, { negative = false, text = "jω" .. name .. "·" .. factor(net, here, there) })
          elseif kind == "L" and domain.ac then
            table.insert(terms, { negative = false, text = factor(net, here, there) .. "/(jω" .. name .. ")" })
          elseif kind == "I" and not part.off then
            -- The source carries its current out of `from`'s node.
            table.insert(terms, { negative = not inA, text = name })
          elseif spec then
            table.insert(terms, { negative = not inA, text = controlText(part, spec) })
          end
        end
      end
    end
    local members = groups[r]
    if #terms > 0 then
      local where
      if #members == 1 then
        where = "KCL at " .. netlist.name(net, members[1])
      else
        local list = table.create(#members)
        for i, node in members do
          list[i] = netlist.name(net, node)
        end
        where = "KCL over the supernode {" .. table.concat(list, ", ") .. "}"
      end
      table.insert(lines, where .. ": " .. sum(terms) .. " = 0")
    end
  end

  -- One constraint per part that fixes a voltage.
  for _, part in parts do
    if usable(part) then
      local kind, name = part.type, model.nameOf(part)
      local a, b = at(part.from), at(part.to)
      local spec = model.DEPENDENT[kind]
      if kind == "V" then
        local value = if part.off then "0 (switched off)" else format.value(phasor.source(part, domain), "V")
        table.insert(lines, name .. " fixes " .. drop(net, b, a) .. " = " .. value)
      elseif kind == "L" and not domain.ac then
        table.insert(lines, name .. " is a short in DC: " .. drop(net, b, a) .. " = 0")
      elseif spec and spec.out == "V" then
        table.insert(lines, name .. " fixes " .. drop(net, b, a) .. " = " .. controlText(part, spec))
      elseif solved.meters[part] then
        table.insert(lines, name .. " is a wire: " .. drop(net, b, a) .. " = 0")
      elseif kind == "opamp" then
        -- The grounded input, if any, goes on the right: "V(B) = 0".
        local left, right = if a == 0 then b else a, if a == 0 then a else b
        table.insert(lines, name .. " (ideal op-amp, no input current) fixes " .. drop(net, left, 0) .. " = " .. drop(net, right, 0))
      end
    end
  end
  if #definitions > 0 then
    table.insert(lines, "where " .. table.concat(definitions, "; "))
  end
  if domain.ac then
    table.insert(lines, "with ω = 2π·" .. format.eng(domain.frequencyHz, "Hz") .. " = " .. format.eng(domain.omega, "rad/s"))
  end
  return lines
end

return equations
