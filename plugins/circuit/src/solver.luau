--!strict
-- Modified nodal analysis. Unknowns: one voltage per node, then one current
-- per voltage source, op-amp output and DC inductor (a 0 V source). Branch
-- currents are reported flowing from `from` to `to` through the part
-- (op-amp: out of the output pin).
local num = require("@harness/num")
local model = require("./model")
local netlist = require("./netlist")
local phasor = require("./phasor")

type Part = model.Part
type Matrix = { { num.Number } }

type System = {
  domain: phasor.Domain,
  net: netlist.Netlist,
  branchOf: { [Part]: number }, -- part → index of its current unknown
  mat: Matrix,
  rhs: { num.Number },
  warnings: { string },
}

-- An ideal op-amp is the large-gain limit.
local OPAMP_GAIN = 1e6

local solver = {}

local function emptySolution(warnings: { string }): model.Solution
  return { probes = {}, hidden = {}, nodes = {}, nodeNames = {}, currents = {}, warnings = warnings }
end

-- Passive parts need a positive value; the others are left out of the solution.
local function rejectInvalid(parts: { Part }, warnings: { string }): { [Part]: boolean }
  local rejected = {}
  for _, part in parts do
    if model.isPassive(part.type) then
      local value = tonumber(part.value)
      if not value or value <= 0 then
        rejected[part] = true
        table.insert(warnings, "`" .. model.nameOf(part) .. "` needs a positive value; it is left out of the solution")
      end
    end
  end
  return rejected
end

-- Adds `v` at (i, j); row or column 0 is ground, which has no unknown.
local function add(mat: Matrix, i: number, j: number, v: num.Number)
  if i > 0 and j > 0 then
    mat[i][j] = num.add(mat[i][j], v)
  end
end

local function stampAdmittance(mat: Matrix, a: number, b: number, y: num.Number)
  local negative = num.mul(y, -1)
  add(mat, a, a, y)
  add(mat, b, b, y)
  add(mat, a, b, negative)
  add(mat, b, a, negative)
end

local function stamp(sys: System, part: Part)
  local kind, mat, rhs, domain = part.type, sys.mat, sys.rhs, sys.domain
  local a = netlist.at(sys.net, part.from) or 0
  local b = netlist.at(sys.net, part.to) or 0
  if kind == "V" and a == b then
    table.insert(sys.warnings, "voltage source `" .. model.nameOf(part) .. "` is shorted")
  end
  if kind == "R" or (kind == "C" and domain.ac) or (kind == "L" and domain.ac) then
    stampAdmittance(mat, a, b, phasor.admittance(kind, tonumber(part.value) :: number, domain))
  elseif kind == "V" or kind == "L" then
    -- Raises `to` above `from` by the value (a DC inductor by 0 V).
    local k = sys.branchOf[part]
    add(mat, a, k, 1)
    add(mat, b, k, -1)
    add(mat, k, a, -1)
    add(mat, k, b, 1)
    if kind == "V" then
      rhs[k] = num.add(rhs[k], phasor.source(part, domain))
    end
  elseif kind == "I" then
    local current = phasor.source(part, domain)
    if a > 0 then
      rhs[a] = num.sub(rhs[a], current)
    end
    if b > 0 then
      rhs[b] = num.add(rhs[b], current)
    end
  elseif kind == "opamp" then
    local k = sys.branchOf[part]
    local out = netlist.at(sys.net, part.third) or 0
    if out == 0 then
      table.insert(sys.warnings, "op-amp `" .. model.nameOf(part) .. "` needs its output at `third`, off ground")
      mat[k][k] = domain.one
    else
      -- Row k: Vout − A(V+ − V−) = 0; the output current enters at `third`.
      add(mat, k, out, 1)
      add(mat, k, a, -OPAMP_GAIN)
      add(mat, k, b, OPAMP_GAIN)
      add(mat, out, k, 1)
    end
  end
end

-- Warns about nodes with fewer than two parts. A node with none would make
-- the matrix singular, so its voltage is pinned to 0.
local function checkConnections(sys: System, parts: { Part }, rejected: { [Part]: boolean })
  local degree: { [number]: number } = {}
  local function touch(node: number?)
    if node and node > 0 then
      degree[node] = (degree[node] or 0) + 1
    end
  end
  for _, part in parts do
    local kind = part.type
    if kind ~= "probe" and kind ~= "ground" and not rejected[part] then
      if not model.isShort(part) then
        touch(netlist.at(sys.net, part.from))
        touch(netlist.at(sys.net, part.to))
      end
      if kind == "opamp" then
        touch(netlist.at(sys.net, part.third))
      end
    end
  end
  for node, name in sys.net.names do
    local count = degree[node] or 0
    if count == 0 then
      table.insert(sys.warnings, "`" .. name .. "` is not connected to anything")
      sys.mat[node][node] = sys.domain.one
    elseif count < 2 then
      table.insert(sys.warnings, "`" .. name .. "` is dangling: only one part connects to it")
    end
  end
end

local function readSolution(sys: System, parts: { Part }, rejected: { [Part]: boolean }, x: num.Vector): model.Solution
  local net, domain = sys.net, sys.domain
  local function voltage(node: number?): num.Number
    return if node and node > 0 then x[node] else 0
  end

  local nodes = {}
  for _, point in net.points do
    nodes[point] = voltage(net.nodeOf[point])
  end
  local nodeNames = {}
  for node, name in net.names do
    nodeNames[name] = x[node]
  end

  local currents = {}
  for _, part in parts do
    local kind = part.type
    if not rejected[part] and (model.isPassive(kind) or kind == "V" or kind == "I" or kind == "opamp") then
      local branch = sys.branchOf[part]
      local current
      if branch then
        current = if kind == "opamp" then num.mul(x[branch], -1) else x[branch]
      elseif kind == "I" then
        current = phasor.source(part, domain)
      else
        local drop = num.sub(voltage(netlist.at(net, part.from)), voltage(netlist.at(net, part.to)))
        current = num.mul(drop, phasor.admittance(kind, tonumber(part.value) :: number, domain))
      end
      currents[model.nameOf(part)] = current
    end
  end

  local probes, hidden = {}, {}
  for _, part in parts do
    if part.type == "probe" then
      local label = model.nameOf(part)
      local at = netlist.at(net, part.from)
      if at == nil then
        table.insert(sys.warnings, "probe `" .. label .. "` is not on a node any part touches")
      else
        probes[label] = num.sub(voltage(at), voltage(netlist.at(net, part.to)))
        if part.hidden then
          hidden[label] = true
        end
      end
    end
  end

  return {
    probes = probes,
    hidden = hidden,
    nodes = nodes,
    nodeNames = nodeNames,
    currents = currents,
    warnings = sys.warnings,
  }
end

function solver.solve(circuit: model.Circuit): model.Solution
  local parts = circuit.components
  local domain = phasor.domain(circuit.analysis)
  local warnings = {}
  local rejected = rejectInvalid(parts, warnings)
  local net = netlist.build(parts, warnings)

  local branchOf: { [Part]: number } = {}
  local size = net.count
  for _, part in parts do
    local kind = part.type
    if not rejected[part] and (kind == "V" or kind == "opamp" or (kind == "L" and not domain.ac)) then
      size += 1
      branchOf[part] = size
    end
  end
  if size == 0 then
    return emptySolution(warnings)
  end

  local mat: Matrix, rhs: { num.Number } = {}, {}
  for i = 1, size do
    mat[i] = table.create(size, domain.zero)
    rhs[i] = domain.zero
  end
  local sys: System = { domain = domain, net = net, branchOf = branchOf, mat = mat, rhs = rhs, warnings = warnings }
  for _, part in parts do
    if not rejected[part] then
      stamp(sys, part)
    end
  end
  checkConnections(sys, parts, rejected)

  local ok, x = pcall(num.linsolve, mat, rhs)
  if not ok then
    table.insert(warnings, "the circuit has no unique solution (check sources and grounds)")
    return emptySolution(warnings)
  end
  return readSolution(sys, parts, rejected, x)
end

return solver
