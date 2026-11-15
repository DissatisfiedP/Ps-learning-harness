--!strict
--[[
  Modified nodal analysis. Unknowns: one voltage per node, then one current
  per voltage source (independent or dependent), op-amp output, DC inductor
  and metered wire (each a 0 V source). Branch currents are reported flowing
  from `from` to `to` through the part (op-amp: out of the output pin), and
  powers as absorbed by the part: (V(from) − V(to)) · I, so a source that
  supplies power reads negative.
]]
local num = require("@harness/num")
local model = require("./model")
local netlist = require("./netlist")
local phasor = require("./phasor")

type Part = model.Part
type Matrix = { { num.Number } }

-- A linear expression over the unknowns: Σ coeff · x[index] + const. Index 0
-- is ground and contributes nothing.
type Term = { index: number, coeff: num.Number }
type Expr = { terms: { Term }, const: num.Number }

type System = {
  domain: phasor.Domain,
  net: netlist.Netlist,
  byName: { [string]: Part },
  rejected: { [Part]: boolean },
  meters: { [Part]: boolean },
  branchOf: { [Part]: number }, -- part → index of its current unknown
  mat: Matrix,
  rhs: { num.Number },
  warnings: { string },
}

-- A solution with what the derived analyses need to read it.
export type Solved = {
  solution: model.Solution,
  net: netlist.Netlist,
  domain: phasor.Domain,
  byName: { [string]: Part },
  rejected: { [Part]: boolean },
  meters: { [Part]: boolean },
  ok: boolean, -- false when the circuit has no unique solution
}

local KNOWN = table.freeze({
  R = true,
  C = true,
  L = true,
  V = true,
  I = true,
  VCVS = true,
  VCCS = true,
  CCVS = true,
  CCCS = true,
  opamp = true,
  ground = true,
  wire = true,
  switch = true,
  probe = true,
  node = true,
  terminal = true,
}) :: { [string]: boolean }

local solver = {}

local function emptySolution(warnings: { string }): model.Solution
  return {
    probes = {},
    units = {},
    hidden = {},
    nodes = {},
    nodeNames = {},
    currents = {},
    powers = {},
    warnings = warnings,
  }
end

-- Parts with a branch current worth reporting.
local function carriesCurrent(sys: System, part: Part): boolean
  local kind = part.type
  return model.isPassive(kind)
    or model.isSource(kind)
    or model.DEPENDENT[kind] ~= nil
    or kind == "opamp"
    or sys.meters[part] == true
end

-- Why a dependent source can't be solved, or nil when it can.
local function dependentProblem(part: Part, spec: model.Dependent, byName: { [string]: Part }, rejected: { [Part]: boolean }): string?
  local control = part.control
  if type(part.gain) ~= "number" then
    return "needs a numeric `gain`"
  elseif type(control) ~= "table" then
    return "needs a `control`"
  end
  local name = control.part
  local target = if name then byName[name] else nil
  if name and not target then
    return "is controlled by `" .. name .. "`, which is not a part"
  elseif target == part then
    return "can't be controlled by itself"
  elseif target and rejected[target] then
    return "is controlled by `" .. (name :: string) .. "`, which is left out"
  elseif target and not model.isTwoTerminal(target.type) then
    return "is controlled by `" .. (name :: string) .. "`, which has no voltage or current of its own"
  elseif spec.by == "I" and not target then
    return "needs `control.part`: the part whose current it follows"
  elseif spec.by == "V" and not target and not model.key(control.plus) then
    return "needs `control.part` or `control.plus`"
  end
  return nil
end

-- Parts that can't be solved are left out, each with a warning.
local function validate(parts: { Part }, byName: { [string]: Part }, warnings: { string }): { [Part]: boolean }
  local rejected: { [Part]: boolean } = {}
  local function reject(part: Part, why: string)
    rejected[part] = true
    table.insert(warnings, "`" .. model.nameOf(part) .. "` " .. why .. "; it is left out of the solution")
  end
  local seen: { [string]: boolean } = {}
  for _, part in parts do
    local kind = part.type
    if not KNOWN[kind] then
      reject(part, "has the unknown type `" .. tostring(kind) .. "`")
    elseif model.isTwoTerminal(kind) and not (model.key(part.from) and model.key(part.to)) then
      reject(part, "needs both `from` and `to`")
    elseif model.isPassive(kind) then
      local value = tonumber(part.value)
      if not value or value <= 0 then
        reject(part, "needs a positive value")
      end
    end
    if model.isTwoTerminal(kind) and kind ~= "wire" and kind ~= "switch" then
      local name = model.nameOf(part)
      if seen[name] then
        table.insert(warnings, "two parts are named `" .. name .. "`; give each its own `id` or `label`")
      end
      seen[name] = true
    end
  end
  -- A source controlled by a rejected part falls with it, so repeat until stable.
  local changed = true
  while changed do
    changed = false
    for _, part in parts do
      local spec = model.DEPENDENT[part.type]
      if spec and not rejected[part] then
        local problem = dependentProblem(part, spec, byName, rejected)
        if problem then
          reject(part, problem)
          changed = true
        end
      end
    end
  end
  return rejected
end

local function node(sys: System, point: model.Point?): number
  return netlist.at(sys.net, point) or 0
end

local function scaled(expr: Expr, factor: num.Number): Expr
  local terms: { Term } = {}
  for i, term in expr.terms do
    terms[i] = { index = term.index, coeff = num.mul(term.coeff, factor) }
  end
  return { terms = terms, const = num.mul(expr.const, factor) }
end

local controlExpr: (sys: System, part: Part, depth: number) -> Expr?

-- The current from `from` to `to` through `part`, or nil when it has none.
local function currentExpr(sys: System, part: Part, depth: number): Expr?
  local kind, domain = part.type, sys.domain
  if sys.rejected[part] then
    return nil
  end
  local branch = sys.branchOf[part]
  if branch then
    return { terms = { { index = branch, coeff = if kind == "opamp" then -1 else 1 } }, const = 0 }
  end
  if kind == "R" or ((kind == "C" or kind == "L") and domain.ac) then
    local y = phasor.admittance(kind, tonumber(part.value) :: number, domain)
    return {
      terms = { { index = node(sys, part.from), coeff = y }, { index = node(sys, part.to), coeff = num.mul(y, -1) } },
      const = 0,
    }
  elseif kind == "C" or kind == "switch" then
    -- A DC capacitor and an open switch carry nothing.
    return { terms = {}, const = 0 }
  elseif kind == "I" then
    return { terms = {}, const = phasor.source(part, domain) }
  elseif kind == "VCCS" or kind == "CCCS" then
    local control = if depth < 8 then controlExpr(sys, part, depth + 1) else nil
    return if control then scaled(control, part.gain :: number) else nil
  end
  return nil
end

-- What a dependent source follows: a voltage or a current, as an expression.
function controlExpr(sys: System, part: Part, depth: number): Expr?
  local spec = model.DEPENDENT[part.type]
  local control = part.control :: model.Control
  local target = if control.part then sys.byName[control.part] else nil
  if spec.by == "I" then
    return if target then currentExpr(sys, target, depth) else nil
  end
  local plus = if target then node(sys, target.from) else node(sys, control.plus)
  local minus = if target then node(sys, target.to) else node(sys, control.minus)
  return { terms = { { index = plus, coeff = 1 }, { index = minus, coeff = -1 } }, const = 0 }
end

local function evaluate(expr: Expr, x: num.Vector): num.Number
  local sum = expr.const
  for _, term in expr.terms do
    if term.index > 0 then
      sum = num.add(sum, num.mul(term.coeff, x[term.index]))
    end
  end
  return sum
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

-- A branch raising `to` above `from`: its current leaves `from`'s node and
-- its row reads V(to) − V(from) = (what the caller adds).
local function stampBranch(mat: Matrix, a: number, b: number, k: number)
  add(mat, a, k, 1)
  add(mat, b, k, -1)
  add(mat, k, a, -1)
  add(mat, k, b, 1)
end

local function stampDependent(sys: System, part: Part, spec: model.Dependent, a: number, b: number)
  local mat, rhs = sys.mat, sys.rhs
  local control = controlExpr(sys, part, 0)
  if not control then
    table.insert(sys.warnings, "`" .. model.nameOf(part) .. "` follows a current that can't be read; it is solved as 0")
    control = { terms = {}, const = 0 }
  end
  local output = scaled(control :: Expr, part.gain :: number)
  if spec.out == "I" then
    -- The current leaves `from`'s node and enters `to`'s.
    for _, term in output.terms do
      add(mat, a, term.index, term.coeff)
      add(mat, b, term.index, num.mul(term.coeff, -1))
    end
    if a > 0 then
      rhs[a] = num.sub(rhs[a], output.const)
    end
    if b > 0 then
      rhs[b] = num.add(rhs[b], output.const)
    end
  else
    local k = sys.branchOf[part]
    stampBranch(mat, a, b, k)
    for _, term in output.terms do
      add(mat, k, term.index, num.mul(term.coeff, -1))
    end
    rhs[k] = num.add(rhs[k], output.const)
  end
end

local function stamp(sys: System, part: Part)
  local kind, mat, rhs, domain = part.type, sys.mat, sys.rhs, sys.domain
  local a, b = node(sys, part.from), node(sys, part.to)
  local name = model.nameOf(part)
  if model.isTwoTerminal(kind) and not model.isShort(part) and a == b then
    if kind == "V" then
      table.insert(sys.warnings, "voltage source `" .. name .. "` is shorted")
    elseif kind ~= "switch" then
      table.insert(sys.warnings, "`" .. name .. "` is shorted: both of its ends are on one node")
    end
  end
  local spec = model.DEPENDENT[kind]
  if spec then
    stampDependent(sys, part, spec, a, b)
  elseif kind == "R" or (kind == "C" and domain.ac) or (kind == "L" and domain.ac) then
    stampAdmittance(mat, a, b, phasor.admittance(kind, tonumber(part.value) :: number, domain))
  elseif kind == "V" or kind == "L" or sys.meters[part] then
    -- Raises `to` above `from` by the value (a DC inductor or a metered wire by 0 V).
    local k = sys.branchOf[part]
    stampBranch(mat, a, b, k)
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
    local out = node(sys, part.third)
    if out == 0 then
      table.insert(sys.warnings, "op-amp `" .. name .. "` needs its output at `third`, off ground")
      mat[k][k] = domain.one
    elseif a == b then
      -- V+ − V− = 0 says nothing then, so the output is left undriven.
      table.insert(sys.warnings, "op-amp `" .. name .. "` has both inputs on one node; its output is left undriven")
      mat[k][k] = domain.one
    else
      -- Ideal: row k reads V+ − V− = 0, and the output supplies whatever
      -- current that takes, entering at `third`.
      add(mat, k, a, 1)
      add(mat, k, b, -1)
      add(mat, out, k, 1)
    end
  end
end

--[[
  Warns about nodes with fewer than two parts. A node that no part conducts to
  (it hangs off an open switch, a DC capacitor or an op-amp input) has no
  equation of its own, so its voltage is pinned to 0 with a warning. One fed
  only by current sources is left alone: that circuit has no solution.
]]
local function checkConnections(sys: System, parts: { Part })
  local degree: { [number]: number } = {}
  local conducting: { [number]: boolean } = {}
  local function touch(at: number?, conducts: boolean)
    if at and at > 0 then
      degree[at] = (degree[at] or 0) + 1
      conducting[at] = conducting[at] or conducts
    end
  end
  local domain = sys.domain
  for _, part in parts do
    local kind = part.type
    if (model.isTwoTerminal(kind) or kind == "opamp") and not sys.rejected[part] then
      if not model.isShort(part) or sys.meters[part] then
        -- An open switch and a DC capacitor join nothing; an op-amp's inputs draw nothing.
        local open = (kind == "switch" and not model.isShort(part)) or kind == "opamp" or (kind == "C" and not domain.ac)
        touch(netlist.at(sys.net, part.from), not open)
        touch(netlist.at(sys.net, part.to), not open)
      end
      if kind == "opamp" then
        touch(netlist.at(sys.net, part.third), true)
      end
    end
  end
  for at, name in sys.net.names do
    local count = degree[at] or 0
    if not conducting[at] then
      table.insert(sys.warnings, "`" .. name .. "` floats: no part conducts to it, so its voltage is taken as 0")
      sys.mat[at][at] = sys.domain.one
    elseif count < 2 and not sys.net.open[at] then
      table.insert(sys.warnings, "`" .. name .. "` is dangling: only one part connects to it")
    end
  end
end

local function readSolution(sys: System, parts: { Part }, x: num.Vector): model.Solution
  local net, domain = sys.net, sys.domain
  local function voltage(at: number?): num.Number
    return if at and at > 0 then x[at] else 0
  end
  local function across(part: Part): num.Number
    return num.sub(voltage(netlist.at(net, part.from)), voltage(netlist.at(net, part.to)))
  end

  local nodes = {}
  for _, point in net.points do
    nodes[point] = voltage(net.nodeOf[point])
  end
  local nodeNames = {}
  for at, name in net.names do
    nodeNames[name] = x[at]
  end

  local currents, powers = {}, {}
  for _, part in parts do
    if not sys.rejected[part] and carriesCurrent(sys, part) then
      local expr = currentExpr(sys, part, 0)
      if expr then
        local name = model.nameOf(part)
        local current = evaluate(expr, x)
        currents[name] = current
        if model.isTwoTerminal(part.type) and not model.isShort(part) then
          powers[name] = num.mul(num.mul(across(part), num.conj(current)), domain.powerScale)
        end
      end
    end
  end

  local probes, units, hidden = {}, {}, {}
  for _, part in parts do
    if part.type == "probe" then
      local label = model.nameOf(part)
      local reading: num.Number? = nil
      local unit = "V"
      local targetName = part.part
      if targetName then
        local target = sys.byName[targetName]
        local name = if target then model.nameOf(target) else targetName
        if not target or sys.rejected[target] or not model.isTwoTerminal(target.type) then
          table.insert(sys.warnings, "probe `" .. label .. "` reads `" .. targetName .. "`, which is not a part in the solution")
        elseif part.quantity == "current" then
          reading, unit = currents[name], "A"
          if reading == nil then
            reading = 0
          end
        elseif part.quantity == "power" then
          local power = powers[name]
          reading, unit = if power ~= nil then num.real(power) else 0, "W"
        else
          reading = across(target)
        end
      else
        local at = netlist.at(net, part.from)
        if at == nil then
          table.insert(sys.warnings, "probe `" .. label .. "` is not on a node any part touches")
        else
          reading = num.sub(voltage(at), voltage(netlist.at(net, part.to)))
        end
      end
      if reading ~= nil then
        probes[label] = reading
        units[label] = unit
        if part.hidden then
          hidden[label] = true
        end
      end
    end
  end

  return {
    probes = probes,
    units = units,
    hidden = hidden,
    nodes = nodes,
    nodeNames = nodeNames,
    currents = currents,
    powers = powers,
    warnings = sys.warnings,
  }
end

-- Solves `parts`. `letters` names the nodes A, B, … instead of describing them.
function solver.run(parts: { Part }, analysis: model.Analysis, letters: boolean): Solved
  local domain = phasor.domain(analysis)
  local warnings = {}
  local byName = model.index(parts)
  local meters = model.meters(parts, byName)
  local rejected = validate(parts, byName, warnings)
  local net = netlist.build(parts, meters, letters, warnings)
  local solved: Solved = {
    solution = emptySolution(warnings),
    net = net,
    domain = domain,
    byName = byName,
    rejected = rejected,
    meters = meters,
    ok = false,
  }

  local branchOf: { [Part]: number } = {}
  local size = net.count
  for _, part in parts do
    local kind = part.type
    local spec = model.DEPENDENT[kind]
    local branch = kind == "V"
      or kind == "opamp"
      or (kind == "L" and not domain.ac)
      or (spec ~= nil and spec.out == "V")
      or meters[part] == true
    if branch and not rejected[part] then
      size += 1
      branchOf[part] = size
    end
  end
  if size == 0 then
    return solved
  end

  local mat: Matrix, rhs: { num.Number } = {}, {}
  for i = 1, size do
    mat[i] = table.create(size, domain.zero)
    rhs[i] = domain.zero
  end
  local sys: System = {
    domain = domain,
    net = net,
    byName = byName,
    rejected = rejected,
    meters = meters,
    branchOf = branchOf,
    mat = mat,
    rhs = rhs,
    warnings = warnings,
  }
  for _, part in parts do
    if not rejected[part] and (model.isTwoTerminal(part.type) or part.type == "opamp") then
      stamp(sys, part)
    end
  end
  checkConnections(sys, parts)

  local ok, x = pcall(num.linsolve, mat, rhs)
  if not ok then
    table.insert(warnings, "the circuit has no unique solution (check sources and grounds)")
    return solved
  end
  solved.solution = readSolution(sys, parts, x)
  solved.ok = true
  return solved
end

function solver.solve(circuit: model.Circuit): Solved
  return solver.run(circuit.components, circuit.analysis, model.letters(circuit))
end

return solver
