--!strict
-- The model-facing text: probes, node voltages, branch currents, warnings.
local num = require("@harness/num")
local model = require("./model")
local format = require("./format")

local report = {}

-- "name = value" pairs, sorted, comma-separated.
local function listing(values: { [string]: num.Number }, unit: string, notes: { [string]: string }?): string
  local lines = {}
  for name, value in values do
    local note = notes and notes[name] or ""
    table.insert(lines, name .. note .. " = " .. format.value(value, unit))
  end
  table.sort(lines)
  return table.concat(lines, ", ")
end

function report.text(analysis: model.Analysis, solution: model.Solution): string
  local hidden = {}
  for name in solution.hidden do
    hidden[name] = " (hidden from the learner)"
  end
  local sections = { "Solved (" .. (if analysis.ac then "AC" else "DC") .. ")." }
  local function section(heading: string, body: string)
    if body ~= "" then
      table.insert(sections, heading .. body .. ".")
    end
  end
  section("Probes: ", listing(solution.probes, "V", hidden))
  section("Nodes (to ground): ", listing(solution.nodeNames, "V"))
  section("Currents (from → to through each part): ", listing(solution.currents, "A"))
  section("Warnings: ", table.concat(solution.warnings, "; "))
  return table.concat(sections, " ")
end

return report
