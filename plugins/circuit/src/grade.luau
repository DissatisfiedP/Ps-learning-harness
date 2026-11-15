--!strict
-- Answer mode: re-solves the submitted circuit rather than trusting values
-- the view computed.
local num = require("@harness/num")
local model = require("./model")
local solver = require("./solver")
local format = require("./format")

-- A probe or a component value to reach: `equals` within `tolerance`
-- (relative, default 0.05) or `floor` (absolute, default 1e-6), whichever is
-- larger. Phasors compare by magnitude.
export type Target = {
  probe: string?,
  component: string?,
  equals: number?,
  tolerance: number?,
  floor: number?,
}

export type Verdict = { correct: boolean, detail: string }

-- `answer` is `{ components, analysis }` as the view holds them.
local function grade(answer: model.Request?, target: Target?): Verdict
  local submitted: model.Request = answer or {}
  local goal: Target = target or {}
  local want = tonumber(goal.equals) or 0
  local tolerance = math.max((tonumber(goal.tolerance) or 0.05) * math.abs(want), tonumber(goal.floor) or 1e-6)
  local function matches(got: num.Number?): boolean
    if got == nil then
      return false
    elseif type(got) == "table" then
      return math.abs(num.abs(got) - math.abs(want)) <= tolerance
    end
    return math.abs(got - want) <= tolerance
  end

  local probe = goal.probe
  if probe then
    local solution = solver.solve(model.circuit(submitted)).solution
    local got = solution.probes[probe]
    local unit = solution.units[probe] or "V"
    local shown = if got ~= nil then format.value(got, unit) else "missing"
    return { correct = matches(got), detail = probe .. " is " .. shown .. "; expected " .. format.eng(want, unit) }
  end

  local component = goal.component
  if component then
    local got: number? = nil
    for _, part in submitted.components or {} do
      if part.id == component then
        -- A dependent source is set by its gain, any other part by its value.
        got = tonumber(if model.DEPENDENT[part.type] then part.gain else part.value)
      end
    end
    local shown = if got then tostring(got) else "missing"
    return { correct = matches(got), detail = component .. " is " .. shown .. "; expected " .. tostring(want) }
  end

  return { correct = false, detail = "the target names neither a probe nor a component" }
end

return grade
