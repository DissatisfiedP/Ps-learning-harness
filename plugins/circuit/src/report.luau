--!strict
-- The model-facing text: probes, node voltages, branch currents, powers,
-- then the derived analyses and the warnings. One section per line.
local num = require("@harness/num")
local model = require("./model")
local netlist = require("./netlist")
local solver = require("./solver")
local analysis = require("./analysis")
local equations = require("./equations")
local format = require("./format")

local report = {}

-- "name = value" pairs, sorted, comma-separated.
local function listing(values: { [string]: string }): string
  local lines = {}
  for name, value in values do
    table.insert(lines, name .. " = " .. value)
  end
  table.sort(lines)
  return table.concat(lines, ", ")
end

local function formatted(values: { [string]: num.Number }, unit: string): { [string]: string }
  local out = {}
  for name, value in values do
    out[name] = format.value(value, unit)
  end
  return out
end

-- Named nodes also list the parts on them: "A [R1, R2] = 3 V".
local function nodeListing(solved: solver.Solved): string
  local net = solved.net
  local out = {}
  for node, name in net.names do
    local value = solved.solution.nodeNames[name]
    if value ~= nil then
      local parts = net.touching[node]
      local shown = if net.described[node] or #parts == 0 then name else name .. " [" .. table.concat(parts, ", ") .. "]"
      out[shown] = format.value(value, "V")
    end
  end
  return listing(out)
end

-- AC powers are complex: "P = 2.5 mW, Q = −2.5 mvar".
local function powerListing(solution: model.Solution, ac: boolean): string
  local out = {}
  for name, power in solution.powers do
    if ac then
      out[name] = format.eng(num.real(power), "W") .. " (Q " .. format.eng(num.imag(power), "var") .. ")"
    else
      out[name] = format.eng(num.real(power), "W")
    end
  end
  return listing(out)
end

function report.text(circuit: model.Circuit, solved: solver.Solved): string
  local solution = solved.solution
  local ac = circuit.analysis.ac
  local probes = {}
  for name, value in solution.probes do
    local note = if solution.hidden[name] then " (hidden from the learner)" else ""
    probes[name .. note] = format.value(value, solution.units[name] or "V")
  end

  local head = "Solved (DC)."
  if ac then
    head = "Solved (AC, f = " .. format.eng(solved.domain.frequencyHz, "Hz") .. "; phasors are " .. (if ac.rms then "RMS" else "peak") .. " values)."
  end
  local sections = { head }
  local function section(heading: string, body: string)
    if body ~= "" then
      table.insert(sections, heading .. body .. ".")
    end
  end
  section("Probes: ", listing(probes))
  section("Nodes (to " .. netlist.name(solved.net, 0) .. "): ", nodeListing(solved))
  section("Currents (from → to through each part): ", listing(formatted(solution.currents, "A")))
  section("Powers (absorbed; negative = supplied): ", powerListing(solution, ac ~= nil))

  if solved.ok then
    for _, line in analysis.groups(solved, circuit.components) do
      table.insert(sections, line)
    end
    local extras = {
      analysis.timeConstant(circuit, solved),
      analysis.equivalent(circuit, solved.byName, solution.warnings),
      analysis.superposition(circuit, solution),
    }
    for i = 1, 3 do
      local line = extras[i]
      if line then
        table.insert(sections, line)
      end
    end
    if circuit.nodal then
      for _, line in equations.nodal(solved, circuit.components) do
        table.insert(sections, line)
      end
    end
  end

  local show = circuit.show
  if next(solution.hidden) ~= nil and (show.voltages or show.currents) then
    table.insert(solution.warnings, "`show.voltages` and `show.currents` are ignored while a probe is hidden: they would give its value away")
  end
  section("Warnings: ", table.concat(solution.warnings, "; "))
  return table.concat(sections, "\n")
end

return report
