--!strict
--[[
  The circuit description shared by the tools, the view and the grader.

  Points are grid coordinates `{ x, y }`; parts whose pins share a point are
  joined. Every part except `ground` and `probe` joins `from` to `to`: `V`
  raises `to` above `from`; `I` pushes its current from `from` to `to`; an
  op-amp has `from` = + input, `to` = − input, `third` = output. `ground` sits
  at `from` only. A `probe` reads V(from) − V(to), with `to` defaulting to
  ground.
]]
local num = require("@harness/num")

export type Point = { number }

export type Part = {
  id: string?,
  type: string,
  from: Point?,
  to: Point?,
  third: Point?,
  value: number?,
  label: string?,
  closed: boolean?,
  hidden: boolean?,
  ac: { magnitude: number?, phaseDeg: number? }?,
}

export type Analysis = { dc: boolean?, ac: { frequencyHz: number }? }

-- What arrives from the model, the view or a submitted answer.
export type Request = { components: { Part }?, analysis: Analysis?, title: string? }

-- A request with its defaults filled in.
export type Circuit = { components: { Part }, analysis: Analysis, title: string? }

export type Solution = {
  probes: { [string]: num.Number }, -- probe label → V(from) − V(to)
  hidden: { [string]: boolean }, -- labels of probes the learner must not see
  nodes: { [string]: num.Number }, -- point key "x,y" → voltage to ground
  nodeNames: { [string]: num.Number }, -- node name → voltage to ground
  currents: { [string]: num.Number }, -- part name → current from `from` to `to`
  warnings: { string },
}

-- A point snapped to whole grid steps.
export type Cell = { x: number, y: number }

local model = {}

-- SI unit of each part type's `value`.
model.UNITS = table.freeze({ R = "Ω", C = "F", L = "H", V = "V", I = "A" }) :: { [string]: string }

function model.circuit(request: Request): Circuit
  return {
    components = request.components or {},
    analysis = request.analysis or { dc = true },
    title = request.title,
  }
end

function model.cell(point: Point?): Cell?
  if type(point) ~= "table" or #point < 2 then
    return nil
  end
  return { x = math.floor(point[1] + 0.5), y = math.floor(point[2] + 0.5) }
end

-- The point's identity in the netlist ("x,y"), or nil when it is not a point.
function model.key(point: Point?): string?
  local cell = model.cell(point)
  return if cell then cell.x .. "," .. cell.y else nil
end

function model.nameOf(part: Part): string
  return part.label or part.id or part.type
end

-- `from`, `to`, then `third` when asked for; a missing pin ends the list.
function model.pins(part: Part, withThird: boolean): { Point }
  local from, to = part.from, part.to
  if not from then
    return {}
  end
  if not to then
    return { from }
  end
  local third = if withThird then part.third else nil
  return if third then { from, to, third } else { from, to }
end

function model.isPassive(kind: string): boolean
  return kind == "R" or kind == "C" or kind == "L"
end

-- Wires and closed switches merge their two points into one node.
function model.isShort(part: Part): boolean
  return part.type == "wire" or (part.type == "switch" and part.closed ~= false)
end

-- What the learner's side may see: hidden probes keep their name, not their value.
function model.public(solution: Solution): Solution
  local probes = {}
  for name, value in solution.probes do
    if not solution.hidden[name] then
      probes[name] = value
    end
  end
  local copy = table.clone(solution)
  copy.probes = probes
  return copy
end

return model
