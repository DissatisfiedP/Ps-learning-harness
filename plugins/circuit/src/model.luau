--!strict
--[[
  The circuit description shared by the tools, the view and the grader.

  Points are grid coordinates `{ x, y }`: x grows to the right, y grows
  upwards. Parts whose pins share a point are joined, and a pin that lies on a
  wire is joined to that wire.

  Two-terminal parts join `from` to `to`:
    - `V` raises `to` above `from`; `I` pushes its current from `from` to `to`;
    - the dependent sources `VCVS`, `CCVS` (voltage out) and `VCCS`, `CCCS`
      (current out) follow the same polarity, with `gain` times their control;
    - a part's own voltage is V(from) − V(to) and its current flows from
      `from` to `to` through it (passive sign convention), which is also what
      a `control` naming that part reads.
  An op-amp has `from` = + input, `to` = − input, `third` = output. `ground`,
  `node`, `terminal` and a point `probe` sit at `from` only.
]]
local num = require("@harness/num")

export type Point = { number }

-- What a dependent source follows: a part (its voltage or its current), or
-- for a voltage-controlled source the two points `plus` and `minus`.
export type Control = { part: string?, plus: Point?, minus: Point?, label: string? }

export type Part = {
  id: string?,
  type: string,
  from: Point?,
  to: Point?,
  third: Point?,
  value: number?,
  gain: number?,
  control: Control?,
  label: string?,
  closed: boolean?,
  toggle: boolean?, -- switch: the learner may open and close it in the view
  hidden: boolean?,
  off: boolean?, -- an independent source switched off: V is a short, I is open
  part: string?, -- probe: the part it reads
  quantity: string?, -- probe: "voltage" (default), "current" or "power"
  ac: { magnitude: number?, phaseDeg: number? }?,
}

export type Analysis = { dc: boolean?, ac: { frequencyHz: number, rms: boolean? }? }

-- What the figure adds to the bare schematic.
export type Show = { nodes: boolean?, voltages: boolean?, currents: boolean?, values: boolean? }

-- A mark drawn over the schematic; it never changes the solution.
export type Annotation = {
  type: string, -- current, voltage, path, loop, cross, highlight, note
  part: string?,
  parts: { string }?,
  from: Point?,
  to: Point?,
  at: Point?,
  points: { Point }?,
  label: string?,
  text: string?,
  reverse: boolean?,
  value: boolean?,
  direction: string?,
  closed: boolean?,
  color: string?,
}

-- The two terminals a Thevenin/Norton equivalent is taken at.
export type Equivalent = { from: Point?, to: Point?, load: string? }

-- What arrives from the model, the view or a submitted answer.
export type Request = {
  components: { Part }?,
  analysis: Analysis?,
  title: string?,
  show: Show?,
  annotations: { Annotation }?,
  equivalent: Equivalent?,
  superposition: boolean?,
  nodal: boolean?,
}

-- A request with its defaults filled in.
export type Circuit = {
  components: { Part },
  analysis: Analysis,
  title: string?,
  show: Show,
  annotations: { Annotation },
  equivalent: Equivalent?,
  superposition: boolean,
  nodal: boolean,
}

export type Solution = {
  probes: { [string]: num.Number }, -- probe label → reading
  units: { [string]: string }, -- probe label → V, A or W
  hidden: { [string]: boolean }, -- labels of probes the learner must not see
  nodes: { [string]: num.Number }, -- point key "x,y" → voltage to ground
  nodeNames: { [string]: num.Number }, -- node name → voltage to ground
  currents: { [string]: num.Number }, -- part name → current from `from` to `to`
  powers: { [string]: num.Number }, -- part name → power absorbed (AC: P + jQ)
  warnings: { string },
}

-- A point snapped to whole grid steps.
export type Cell = { x: number, y: number }

-- A dependent source: what it puts out, what it follows, the unit of `gain`.
export type Dependent = { out: "V" | "I", by: "V" | "I", unit: string }

local model = {}

-- SI unit of each part type's `value`.
model.UNITS = table.freeze({ R = "Ω", C = "F", L = "H", V = "V", I = "A" }) :: { [string]: string }

model.DEPENDENT = table.freeze({
  VCVS = { out = "V", by = "V", unit = "" },
  VCCS = { out = "I", by = "V", unit = "S" },
  CCVS = { out = "V", by = "I", unit = "Ω" },
  CCCS = { out = "I", by = "I", unit = "" },
}) :: { [string]: Dependent }

local TWO_TERMINAL = table.freeze({
  R = true,
  C = true,
  L = true,
  V = true,
  I = true,
  VCVS = true,
  VCCS = true,
  CCVS = true,
  CCCS = true,
  wire = true,
  switch = true,
}) :: { [string]: boolean }

local ONE_POINT = table.freeze({ ground = true, probe = true, node = true, terminal = true }) :: { [string]: boolean }

function model.circuit(request: Request): Circuit
  return {
    components = request.components or {},
    analysis = request.analysis or { dc = true },
    title = request.title,
    show = request.show or {},
    annotations = request.annotations or {},
    equivalent = request.equivalent,
    superposition = request.superposition == true,
    nodal = request.nodal == true,
  }
end

-- True when the figure or the text names every node with a letter.
function model.letters(circuit: Circuit): boolean
  return circuit.nodal or circuit.show.nodes == true or circuit.show.voltages == true
end

function model.cell(point: Point?): Cell?
  if type(point) ~= "table" or type(point[1]) ~= "number" or type(point[2]) ~= "number" then
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

-- Parts by the names other parts refer to them with: `id` first, then `label`.
function model.index(parts: { Part }): { [string]: Part }
  local byName: { [string]: Part } = {}
  for _, part in parts do
    local label = part.label
    if label and part.type ~= "probe" and byName[label] == nil then
      byName[label] = part
    end
  end
  for _, part in parts do
    local id = part.id
    if id then
      byName[id] = part
    end
  end
  return byName
end

-- `from`, `to`, then `third` when asked for; a missing pin ends the list.
function model.pins(part: Part, withThird: boolean): { Point }
  local from, to = part.from, part.to
  if not from then
    return {}
  end
  if not to or ONE_POINT[part.type] then
    return { from }
  end
  local third = if withThird then part.third else nil
  return if third then { from, to, third } else { from, to }
end

function model.isPassive(kind: string): boolean
  return kind == "R" or kind == "C" or kind == "L"
end

-- Parts drawn and solved between `from` and `to`.
function model.isTwoTerminal(kind: string): boolean
  return TWO_TERMINAL[kind] == true
end

-- An independent source.
function model.isSource(kind: string): boolean
  return kind == "V" or kind == "I"
end

-- Wires and closed switches merge their two points into one node.
function model.isShort(part: Part): boolean
  return part.type == "wire" or (part.type == "switch" and part.closed ~= false)
end

-- Wires and closed switches whose current something reads (a probe, or a
-- current-controlled source). They are solved as 0 V sources instead of
-- merging their ends, so that current exists.
function model.meters(parts: { Part }, byName: { [string]: Part }): { [Part]: boolean }
  local meters: { [Part]: boolean } = {}
  local function mark(name: string?)
    local target = if name then byName[name] else nil
    if target and model.isShort(target) then
      meters[target] = true
    end
  end
  for _, part in parts do
    local spec = model.DEPENDENT[part.type]
    local control = part.control
    if spec and spec.by == "I" and control then
      mark(control.part)
    elseif part.type == "probe" then
      mark(part.part)
    end
  end
  return meters
end

--[[
  What the learner's side may see. A hidden probe keeps its name, not its
  value; and because any node voltage or branch current would give a hidden
  value away, they are all withheld while a probe is hidden.
]]
function model.public(solution: Solution): Solution
  local copy = table.clone(solution)
  if next(solution.hidden) == nil then
    return copy
  end
  local probes, units = {}, {}
  for name, value in solution.probes do
    if not solution.hidden[name] then
      probes[name] = value
      units[name] = solution.units[name]
    end
  end
  copy.probes = probes
  copy.units = units
  copy.nodes = {}
  copy.nodeNames = {}
  copy.currents = {}
  copy.powers = {}
  return copy
end

return model
