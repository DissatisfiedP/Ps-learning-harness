--!strict
--[[
  Lays the parts out on a pixel grid and draws the whole figure: the circuit,
  then what `show` and `annotations` add to it, then the labels.

  Grid y grows upwards, so it is flipped on the way to the page. Parts that
  join the same two points are parallel branches: they are spread side by
  side instead of being drawn over each other.
]]
local num = require("@harness/num")
local canvas = require("./canvas")
local model = require("./model")
local netlist = require("./netlist")
local symbols = require("./symbols")
local marks = require("./marks")
local format = require("./format")

type Part = model.Part
type Placement = symbols.Placement
type Px = symbols.Px

-- A current arrow or a voltage polarity on a part.
type Mark = {
  label: string?,
  reading: string?, -- a value given as text (a probe's), instead of one looked up
  reverse: boolean,
  value: boolean, -- look the solved value up and show it
  color: string,
}

local GRID = 70 -- px per grid step
local SPREAD = 70 -- px between neighbouring parallel parts
local OFF_OPACITY = 0.4

local schematic = {}

-- A point given in grid steps (fractions allowed), on the page.
local function page(point: model.Point?): Px?
  if type(point) ~= "table" or type(point[1]) ~= "number" or type(point[2]) ~= "number" then
    return nil
  end
  return { x = point[1] * GRID, y = -point[2] * GRID }
end

-- A part's pin, snapped to the grid, on the page.
local function pin(point: model.Point?): Px?
  local cell = model.cell(point)
  return if cell then { x = cell.x * GRID, y = -cell.y * GRID } else nil
end

local function keyPx(key: string): Px?
  local x, y = key:match("^(-?%d+),(-?%d+)$")
  return if x and y then { x = (tonumber(x) :: number) * GRID, y = -(tonumber(y) :: number) * GRID } else nil
end

-- Where every two-terminal part goes, parallel parts spread apart.
local function place(parts: { Part }): ({ [Part]: Placement }, { Placement })
  local groups: { [string]: { Part } } = {}
  local function pairOf(part: Part): string?
    local ka, kb = model.key(part.from), model.key(part.to)
    if not ka or not kb or ka == kb or not model.isTwoTerminal(part.type) then
      return nil
    end
    return if ka < kb then ka .. "|" .. kb else kb .. "|" .. ka
  end
  for _, part in parts do
    local pair = pairOf(part)
    if pair then
      groups[pair] = groups[pair] or {}
      table.insert(groups[pair], part)
    end
  end

  local placed: { [Part]: Placement } = {}
  local order: { Placement } = {}
  for _, part in parts do
    local pair = pairOf(part)
    local a, b = pin(part.from), pin(part.to)
    if pair and a and b then
      local group = groups[pair]
      local len = math.sqrt((b.x - a.x) ^ 2 + (b.y - a.y) ^ 2)
      local ux, uy = (b.x - a.x) / len, (b.y - a.y) / len
      local nx, ny = -uy, ux
      -- Every part of a group is spread along the same normal, whichever way it points.
      local sameWay = (model.key(part.from) :: string) < (model.key(part.to) :: string)
      local shift = ((table.find(group, part) :: number) - (#group + 1) / 2) * SPREAD * (if sameWay then 1 else -1)
      local half, wide = symbols.size(part.type)
      local p: Placement = {
        part = part,
        ax = a.x,
        ay = a.y,
        bx = b.x,
        by = b.y,
        mx = (a.x + b.x) / 2 + nx * shift,
        my = (a.y + b.y) / 2 + ny * shift,
        ux = ux,
        uy = uy,
        nx = nx,
        ny = ny,
        len = len,
        half = half,
        wide = wide,
        horizontal = math.abs(ux) >= math.abs(uy),
        shift = shift,
      }
      placed[part] = p
      table.insert(order, p)
    end
  end
  return placed, order
end

-- The box around some points, grown by `pad`.
local function box(points: { Px }, pad: number): (number, number, number, number)
  local x1, y1, x2, y2 = math.huge, math.huge, -math.huge, -math.huge
  for _, p in points do
    x1 = math.min(x1, p.x)
    y1 = math.min(y1, p.y)
    x2 = math.max(x2, p.x)
    y2 = math.max(y2, p.y)
  end
  return x1 - pad, y1 - pad, x2 + pad, y2 + pad
end

local function joined(label: string?, reading: string?, separator: string): string?
  return if label and reading then label .. separator .. reading else label or reading
end

-- `result` holds the solved values the figure may show; nil shows none.
function schematic.draw(circuit: model.Circuit, result: model.Solution?): string
  local parts = circuit.components
  local show = circuit.show
  local ac = circuit.analysis.ac ~= nil
  local byName = model.index(parts)
  local net = netlist.build(parts, model.meters(parts, byName), model.letters(circuit), {})
  local placed, order = place(parts)
  local c = canvas.new()

  local function placementOf(name: string?): Placement?
    local part = if name then byName[name] else nil
    return if part then placed[part] else nil
  end
  local function voltageAt(point: model.Point?): num.Number?
    local key = model.key(point)
    return if result and key then result.nodes[key] else nil
  end
  -- The pins and centres of the named parts, for a region around them.
  local function corners(names: { string }?): { Px }
    local points = {}
    for _, name in names or {} do
      local p = placementOf(name)
      if p then
        table.insert(points, { x = p.ax, y = p.ay })
        table.insert(points, { x = p.bx, y = p.by })
        table.insert(points, { x = p.mx, y = p.my })
      end
    end
    return points
  end
  local function region(annotation: model.Annotation): { Px }
    local a, b = page(annotation.from), page(annotation.to)
    local points = corners(annotation.parts)
    local single = placementOf(annotation.part)
    if single then
      table.insert(points, { x = single.ax, y = single.ay })
      table.insert(points, { x = single.bx, y = single.by })
    end
    if a and b then
      table.insert(points, a)
      table.insert(points, b)
    end
    return points
  end

  -- The voltage from `a` (+) to `b` (−): marked on the part that spans exactly
  -- those two points when there is one, and beside the gap otherwise.
  local function across(a: Px?, b: Px?, text: string?, color: string)
    if not a or not b then
      return
    end
    for _, p in order do
      local same = p.ax == a.x and p.ay == a.y and p.bx == b.x and p.by == b.y
      local flipped = p.ax == b.x and p.ay == b.y and p.bx == a.x and p.by == a.y
      if (same or flipped) and not model.isShort(p.part) then
        marks.voltage(c, p, text, flipped, color)
        return
      end
    end
    marks.between(c, a.x, a.y, b.x, b.y, text, color)
  end

  -- Under the circuit: current paths and highlighted regions. Their labels
  -- wait until the circuit is drawn, so they can keep off it.
  local late: { { spots: { canvas.Spot }, text: string, color: string } } = {}
  for _, annotation in circuit.annotations do
    local spots: { canvas.Spot }? = nil
    local color = marks.CURRENT
    if annotation.type == "path" then
      local points = {}
      for _, point in annotation.points or {} do
        local at = page(point)
        if at then
          table.insert(points, at.x)
          table.insert(points, at.y)
        end
      end
      color = marks.color(annotation.color, marks.CURRENT)
      spots = marks.path(c, points, annotation.closed == true, color)
    elseif annotation.type == "highlight" then
      local points = region(annotation)
      if #points > 0 then
        local x1, y1, x2, y2 = box(points, 28)
        color = marks.color(annotation.color, marks.REGION)
        spots = marks.highlight(c, x1, y1, x2, y2, color)
      end
    end
    local label = annotation.label
    if spots and #spots > 0 and label then
      table.insert(late, { spots = spots, text = label, color = color })
    end
  end

  -- The circuit itself. A source switched off is faded and crossed out.
  local function isOff(part: Part): boolean
    return part.off == true and model.isSource(part.type)
  end
  for _, p in order do
    if isOff(p.part) then
      canvas.layer(c, { opacity = OFF_OPACITY }, function(layer)
        symbols.draw(layer, p)
      end)
    else
      symbols.draw(c, p)
    end
  end
  for _, part in parts do
    local a = pin(part.from)
    if a and part.type == "ground" then
      symbols.ground(c, a)
    elseif a and part.type == "opamp" then
      local b = pin(part.to)
      if b then
        symbols.opamp(c, part, a, b, pin(part.third))
      end
    end
  end
  for _, point in net.points do
    local at = keyPx(point)
    if at and (net.ends[point] or 0) >= 3 then
      symbols.dot(c, at, 3)
    end
  end

  -- Marks whose place is given: mesh loops and notes. Everything drawn after
  -- them that has a choice keeps off them.
  for _, annotation in circuit.annotations do
    local at = page(annotation.at)
    if at and annotation.type == "loop" then
      local color = marks.color(annotation.color, marks.CURRENT)
      marks.loop(c, at.x, at.y, annotation.label, annotation.direction ~= "ccw", color)
    elseif at and annotation.type == "note" then
      local text = annotation.text or annotation.label
      if text then
        marks.note(c, at.x, at.y, text, if annotation.color then marks.color(annotation.color, "currentColor") else nil)
      end
    end
  end

  -- Captions, once every body is drawn.
  local values = show.values ~= false
  for _, p in order do
    if isOff(p.part) then
      canvas.layer(c, { opacity = OFF_OPACITY }, function(layer)
        symbols.caption(layer, p, ac, values, byName)
      end)
      marks.cross(c, p, marks.VOLTAGE)
    else
      symbols.caption(c, p, ac, values, byName)
    end
  end

  -- Arrows and polarities on parts, from four sources. Later ones fill in
  -- what earlier ones left open.
  local currents: { [Part]: Mark } = {}
  local voltages: { [Part]: Mark } = {}
  local tags: { [Part]: string } = {}
  for _, annotation in circuit.annotations do
    local target = if annotation.part then byName[annotation.part] else nil
    if target and placed[target] and (annotation.type == "current" or annotation.type == "voltage") then
      local isCurrent = annotation.type == "current"
      local mark: Mark = {
        label = annotation.label,
        reverse = annotation.reverse == true,
        value = annotation.value == true,
        color = marks.color(annotation.color, if isCurrent then marks.CURRENT else marks.VOLTAGE),
      }
      if isCurrent then
        currents[target] = mark
      else
        voltages[target] = mark
      end
    end
  end
  for _, part in parts do
    local spec = model.DEPENDENT[part.type]
    local control = part.control
    local target = if spec and control and control.part then byName[control.part] else nil
    if spec and target and placed[target] then
      local map = if spec.by == "I" then currents else voltages
      local label = symbols.controlLabel(part, byName)
      local mark = map[target]
      if not mark then
        map[target] = { label = label, reverse = false, value = false, color = if spec.by == "I" then marks.CURRENT else marks.VOLTAGE }
      elseif not mark.label then
        mark.label = label
      end
    elseif part.type == "probe" and part.part then
      local of = byName[part.part]
      if of and placed[of] then
        local label = model.nameOf(part)
        local value = if result then result.probes[label] else nil
        local reading: string? = nil
        if part.hidden then
          reading = "?"
        elseif value ~= nil and result then
          reading = format.value(value, result.units[label] or "V")
        end
        if part.quantity == "current" then
          currents[of] = { label = label, reading = reading, reverse = false, value = false, color = marks.CURRENT }
        elseif part.quantity == "power" then
          tags[of] = joined(label, reading, " = ") :: string
        else
          voltages[of] = { label = label, reading = reading, reverse = false, value = false, color = marks.VOLTAGE }
        end
      end
    end
  end
  if show.currents and result then
    for _, p in order do
      local current = result.currents[model.nameOf(p.part)]
      local mark = currents[p.part]
      if mark then
        mark.value = true
      elseif current ~= nil then
        -- A DC arrow points the way the current really flows.
        local backwards = type(current) == "number" and current < 0
        currents[p.part] = { reverse = backwards, value = true, color = marks.CURRENT }
      end
    end
  end

  -- Marks on parts, in the order they get to choose their place: every
  -- current first (it belongs on its lead), then the voltages, then powers.
  for _, p in order do
    local current = currents[p.part]
    if current then
      local reading = current.reading
      local solved = if result then result.currents[model.nameOf(p.part)] else nil
      if not reading and current.value and solved ~= nil then
        reading = format.value(if current.reverse then num.mul(solved, -1) else solved, "A")
      end
      marks.current(c, p, joined(current.label, reading, " = "), current.reverse, current.color)
    end
  end
  -- Vertical parts first: their polarity signs have one place, which the
  -- marks of horizontal parts can then keep off.
  for _, upright in { true, false } do
    for _, p in order do
      local part = p.part
      local voltage = voltages[part]
      if voltage and p.horizontal ~= upright then
        local reading = voltage.reading
        local va, vb = voltageAt(part.from), voltageAt(part.to)
        if not reading and voltage.value and va ~= nil and vb ~= nil then
          local drop = num.sub(va, vb)
          reading = format.value(if voltage.reverse then num.mul(drop, -1) else drop, "V")
        end
        marks.voltage(c, p, joined(voltage.label, reading, " = "), voltage.reverse, voltage.color)
      end
    end
  end
  -- Voltages between two points: annotations, then what dependent sources
  -- that follow two points follow.
  for _, annotation in circuit.annotations do
    if annotation.type == "voltage" and not annotation.part then
      local va, vb = voltageAt(annotation.from), voltageAt(annotation.to)
      local reading = if annotation.value and va ~= nil and vb ~= nil then format.value(num.sub(va, vb), "V") else nil
      local color = marks.color(annotation.color, marks.VOLTAGE)
      across(page(annotation.from), page(annotation.to), joined(annotation.label, reading, " = "), color)
    end
  end
  for _, part in parts do
    local spec = model.DEPENDENT[part.type]
    local control = part.control
    if spec and spec.by == "V" and control and not control.part then
      across(pin(control.plus), pin(control.minus), symbols.controlLabel(part, byName), marks.VOLTAGE)
    end
  end
  for _, p in order do
    local tag = tags[p.part]
    if tag then
      marks.tag(c, p, tag, marks.POWER)
    end
  end

  -- Labels at points: named nodes, terminals, probes, then letters for the
  -- nodes nothing names. Every dot is drawn before any label, so a label
  -- keeps off its neighbours' dots too.
  local labels: { { at: Px, text: string, color: string? } } = {}
  local function addLabel(at: Px, text: string, color: string?)
    table.insert(labels, { at = at, text = text, color = color })
  end
  local function withVoltage(name: string, point: model.Point?): string
    local value = if show.voltages then voltageAt(point) else nil
    return if value ~= nil then name .. ": " .. format.value(value, "V") else name
  end
  for _, part in parts do
    local at = pin(part.from)
    local kind = part.type
    if at and kind == "node" then
      symbols.dot(c, at, 3, marks.NODE)
      if part.label then
        addLabel(at, withVoltage(part.label, part.from), marks.NODE)
      end
    elseif at and kind == "terminal" then
      symbols.terminal(c, at)
      if part.label then
        addLabel(at, withVoltage(part.label, part.from), nil)
      end
    elseif at and kind == "probe" and not part.part then
      local name = model.nameOf(part)
      local value = if result then result.probes[name] else nil
      local reading: string? = nil
      if part.hidden then
        reading = "?"
      elseif value ~= nil then
        reading = format.value(value, "V")
      end
      symbols.dot(c, at, 4)
      addLabel(at, joined(name, reading, ": ") :: string, nil)
      -- A probe between two nodes also marks its reference.
      local reference, referenceKey = pin(part.to), model.key(part.to)
      if reference and referenceKey and referenceKey ~= model.key(part.from) and (net.nodeOf[referenceKey] or 0) ~= 0 then
        symbols.dot(c, reference, 2.5)
        addLabel(reference, "−", nil)
      end
    end
  end
  if model.letters(circuit) then
    for node = 1, net.count do
      local key = net.rep[node]
      local at = if key then keyPx(key) else nil
      if at and not net.labelled[node] then
        local value = if show.voltages and result then result.nodes[key] else nil
        local text = if value ~= nil then net.names[node] .. ": " .. format.value(value, "V") else net.names[node]
        symbols.dot(c, at, 3, marks.NODE)
        addLabel(at, text, marks.NODE)
      end
    end
  end
  for _, entry in labels do
    marks.label(c, entry.at.x, entry.at.y, entry.text, entry.color)
  end
  for _, entry in late do
    canvas.place(c, entry.spots, entry.text, { fill = entry.color })
  end

  -- Over everything: what is crossed out.
  for _, annotation in circuit.annotations do
    if annotation.type == "cross" then
      local color = marks.color(annotation.color, marks.VOLTAGE)
      local crossed = false
      local names = table.clone(annotation.parts or {})
      if annotation.part then
        table.insert(names, annotation.part)
      end
      for _, name in names do
        local p = placementOf(name)
        if p then
          marks.cross(c, p, color)
          crossed = true
        end
      end
      local a, b = page(annotation.from), page(annotation.to)
      if not crossed and a and b then
        local x1, y1, x2, y2 = box({ a, b }, 14)
        marks.crossBox(c, x1, y1, x2, y2, color)
      end
    end
  end

  if ac and #c.out > 0 then
    local frequency = tonumber((circuit.analysis.ac :: { frequencyHz: number }).frequencyHz) or 0
    canvas.text(c, c.minX, c.maxY + 18, "f = " .. format.eng(frequency, "Hz"), "start")
  end
  return canvas.document(c)
end

return schematic
