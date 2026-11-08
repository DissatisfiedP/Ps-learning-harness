--!strict
-- One schematic symbol per part (IEC 60617: a resistor is a rectangle), in
-- pixel coordinates. Every function appends SVG elements to `out`.
local svg = require("@harness/svg")
local num = require("@harness/num")
local model = require("./model")
local format = require("./format")

export type Px = { x: number, y: number }

local STROKE = { ["stroke-width"] = 1.5 }
local WIRE = { ["stroke-width"] = 1.5, ["stroke-linecap"] = "round" }
local LEAD = { ["stroke-width"] = 1.5, ["stroke-linecap"] = "round", ["stroke-linejoin"] = "round" }
local DOT = { fill = "currentColor", stroke = "none" }

local function textStyle(anchor: string): svg.Attributes
  return {
    ["font-size"] = 12,
    ["font-family"] = "system-ui, sans-serif",
    fill = "currentColor",
    ["text-anchor"] = anchor,
    stroke = "none",
  }
end
local TEXT = { middle = textStyle("middle"), start = textStyle("start") }

local symbols = {}

local function line(out: { string }, ax: number, ay: number, bx: number, by: number)
  table.insert(out, svg.line(ax, ay, bx, by, WIRE))
end

local function text(out: { string }, x: number, y: number, value: string, anchor: "middle" | "start")
  table.insert(out, svg.text(x, y, value, TEXT[anchor]))
end

-- A wire, or a switch: a wire with a blade drawn over its middle.
local function conductor(out: { string }, part: model.Part, a: Px, b: Px)
  line(out, a.x, a.y, b.x, b.y)
  if part.type == "switch" then
    local mx, my = (a.x + b.x) / 2, (a.y + b.y) / 2
    local dx, dy = (b.x - a.x) * 0.25, (b.y - a.y) * 0.25
    line(out, mx - dx, my - dy, mx + dx, my - dy)
    line(out, mx + dx, my - dy, mx + dx * 2, my + dy * 2)
  end
end

local function ground(out: { string }, a: Px)
  local x, y = a.x, a.y
  line(out, x, y, x, y + 12)
  line(out, x - 12, y + 12, x + 12, y + 12)
  line(out, x - 8, y + 18, x + 8, y + 18)
  line(out, x - 4, y + 24, x + 4, y + 24)
end

local function resistor(out: { string }, a: Px, b: Px, mx: number, my: number, horizontal: boolean)
  if horizontal then
    line(out, a.x, a.y, mx - 16, my)
    line(out, mx + 16, my, b.x, b.y)
    table.insert(out, svg.rect(mx - 16, my - 8, 32, 16, STROKE))
  else
    line(out, a.x, a.y, mx, my - 16)
    line(out, mx, my + 16, b.x, b.y)
    table.insert(out, svg.rect(mx - 8, my - 16, 16, 32, STROKE))
  end
end

local function capacitor(out: { string }, a: Px, b: Px, mx: number, my: number, horizontal: boolean)
  if horizontal then
    line(out, a.x, a.y, mx - 4, my)
    line(out, mx + 4, my, b.x, b.y)
    line(out, mx - 4, my - 12, mx - 4, my + 12)
    line(out, mx + 4, my - 12, mx + 4, my + 12)
  else
    line(out, a.x, a.y, mx, my - 4)
    line(out, mx, my + 4, b.x, b.y)
    line(out, mx - 12, my - 4, mx + 12, my - 4)
    line(out, mx - 12, my + 4, mx + 12, my + 4)
  end
end

local function inductor(out: { string }, a: Px, b: Px, mx: number, my: number, horizontal: boolean)
  if horizontal then
    line(out, a.x, a.y, mx - 24, my)
    line(out, mx + 24, my, b.x, b.y)
    local loops = string.format("M %g %g q 6 -14 12 0 q 6 -14 12 0 q 6 -14 12 0 q 6 -14 12 0", mx - 24, my)
    table.insert(out, svg.path(loops, STROKE))
  else
    line(out, a.x, a.y, mx, my - 24)
    line(out, mx, my + 24, b.x, b.y)
    local loops = string.format("M %g %g q 14 6 0 12 q 14 6 0 12 q 14 6 0 12 q 14 6 0 12", mx, my - 24)
    table.insert(out, svg.path(loops, STROKE))
  end
end

-- A circle with leads to it, so the source visibly joins its two nodes: `+`
-- and `−` for V (raising `to` above `from`), an arrow from `from` to `to` for I.
local function source(out: { string }, part: model.Part, a: Px, b: Px, mx: number, my: number, horizontal: boolean)
  if horizontal then
    local dir = if b.x >= a.x then 1 else -1
    line(out, a.x, a.y, mx - 16 * dir, my)
    line(out, mx + 16 * dir, my, b.x, b.y)
  else
    local dir = if b.y >= a.y then 1 else -1
    line(out, a.x, a.y, mx, my - 16 * dir)
    line(out, mx, my + 16 * dir, b.x, b.y)
  end
  table.insert(out, svg.circle(mx, my, 16, STROKE))
  local len = math.max(1, math.sqrt((b.x - a.x) ^ 2 + (b.y - a.y) ^ 2))
  local ux, uy = (b.x - a.x) / len, (b.y - a.y) / len
  if part.type == "V" then
    text(out, mx + ux * 8, my + uy * 8 + 4, "+", "middle")
    text(out, mx - ux * 8, my - uy * 8 + 4, "−", "middle")
  else
    local angle = math.deg(math.atan2(uy, ux))
    table.insert(
      out,
      svg.path(string.format("M %g %g l 10 0 m -4 -4 l 4 4 l -4 4", mx - 5, my), {
        ["stroke-width"] = 1.5,
        transform = string.format("rotate(%g %g %g)", angle, mx, my),
      })
    )
  end
end

-- A triangle right of the inputs, − pin above + pin, output at the apex.
-- Leads run vertically then horizontally from each point to its pin.
local function opamp(out: { string }, part: model.Part, plus: Px, minus: Px, output: Px?)
  local x0, ym = math.max(plus.x, minus.x) + 20, (plus.y + minus.y) / 2
  local minusY, plusY, apexX = ym - 12, ym + 12, x0 + 40
  table.insert(out, svg.path(string.format("M %g %g l 0 48 l 40 -24 z", x0, ym - 24), STROKE))
  table.insert(out, svg.path(string.format("M %g %g V %g H %g", plus.x, plus.y, plusY, x0), LEAD))
  table.insert(out, svg.path(string.format("M %g %g V %g H %g", minus.x, minus.y, minusY, x0), LEAD))
  if output then
    table.insert(out, svg.path(string.format("M %g %g H %g V %g", apexX, ym, output.x, output.y), LEAD))
  end
  text(out, x0 + 4, minusY + 4, "−", "start")
  text(out, x0 + 4, plusY + 4, "+", "start")
  text(out, x0 + 20, ym - 30, model.nameOf(part), "middle")
end

local function probe(out: { string }, part: model.Part, at: Px, probes: { [string]: num.Number }?)
  local label = model.nameOf(part)
  table.insert(out, svg.circle(at.x, at.y, 4, DOT))
  local value = probes and probes[label]
  local shown = ""
  if part.hidden then
    shown = ": ?"
  elseif value then
    shown = ": " .. format.value(value, "V")
  end
  text(out, at.x + 8, at.y - 8, label .. shown, "start")
end

-- Name and value, beside a vertical part or above a horizontal one: never
-- over the symbol.
local function caption(out: { string }, part: model.Part, mx: number, my: number, horizontal: boolean)
  local value = tonumber(part.value)
  local unit = model.UNITS[part.type]
  local label = model.nameOf(part)
  if value and unit then
    label ..= " " .. format.eng(value, unit)
  end
  if horizontal then
    text(out, mx, my - 22, label, "middle")
  else
    text(out, mx + 24, my + 4, label, "start")
  end
end

-- Draws `part` between `a` (its `from`) and `b` (its `to`; `a` again for a
-- one-point part); `c` is its `third` point, if any. `probes` holds the values
-- probe labels show.
function symbols.draw(out: { string }, part: model.Part, a: Px, b: Px, c: Px?, probes: { [string]: num.Number }?)
  local kind = part.type
  if kind == "wire" or kind == "switch" then
    return conductor(out, part, a, b)
  elseif kind == "ground" then
    return ground(out, a)
  elseif kind == "opamp" then
    return opamp(out, part, a, b, c)
  elseif kind == "probe" then
    return probe(out, part, a, probes)
  end
  local mx, my = (a.x + b.x) / 2, (a.y + b.y) / 2
  local horizontal = math.abs(b.y - a.y) < 1
  if kind == "R" then
    resistor(out, a, b, mx, my, horizontal)
  elseif kind == "C" then
    capacitor(out, a, b, mx, my, horizontal)
  elseif kind == "L" then
    inductor(out, a, b, mx, my, horizontal)
  elseif kind == "V" or kind == "I" then
    source(out, part, a, b, mx, my, horizontal)
  end
  caption(out, part, mx, my, horizontal)
end

return symbols
