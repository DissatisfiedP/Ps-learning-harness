--!strict
--[[
  One schematic symbol per part, in pixel coordinates (ANSI style: a resistor
  is a zigzag, a dependent source a diamond). A two-terminal symbol is drawn
  along its own axis, so it reads the same at any angle; its caption stays
  upright, above a horizontal part and to the right of a vertical one.
]]
local canvas = require("./canvas")
local model = require("./model")
local format = require("./format")

export type Px = { x: number, y: number }

-- Where a two-terminal part is drawn.
export type Placement = {
  part: model.Part,
  ax: number, -- the `from` pin
  ay: number,
  bx: number, -- the `to` pin
  by: number,
  mx: number, -- the symbol's centre
  my: number,
  ux: number, -- unit vector from `from` to `to`
  uy: number,
  nx: number, -- unit normal to it
  ny: number,
  len: number, -- pin to pin
  half: number, -- the body reaches this far from the centre along the axis
  wide: number, -- and this far across it
  horizontal: boolean,
  shift: number, -- px the symbol is moved off the line between its pins (parallel parts)
}

local STROKE = { ["stroke-width"] = 1.5, ["stroke-linejoin"] = "round" }
local WIRE = { ["stroke-width"] = 1.5, ["stroke-linecap"] = "round" }
local LEAD = { ["stroke-width"] = 1.5, ["stroke-linecap"] = "round", ["stroke-linejoin"] = "round" }
local DOT = { fill = "currentColor", stroke = "none" }

-- Half-length and half-width of each body, px.
local SIZES: { [string]: { number } } = {
  R = { 18, 7 },
  C = { 4, 12 },
  L = { 24, 12 },
  V = { 16, 16 },
  I = { 16, 16 },
  VCVS = { 17, 17 },
  VCCS = { 17, 17 },
  CCVS = { 17, 17 },
  CCCS = { 17, 17 },
  switch = { 12, 9 },
  wire = { 0, 0 },
}

local symbols = {}

-- The body's half-length and half-width.
function symbols.size(kind: string): (number, number)
  local size = SIZES[kind] or SIZES.wire
  return size[1], size[2]
end

-- The point `t` px along the axis from the centre and `s` px across it.
function symbols.at(p: Placement, t: number, s: number): (number, number)
  return p.mx + p.ux * t + p.nx * s, p.my + p.uy * t + p.ny * s
end
local at = symbols.at

-- +1 or −1: the side of the axis that faces up (horizontal) or right (vertical).
local function outward(p: Placement): number
  local d = if p.horizontal then -p.ny else p.nx
  return if d >= 0 then 1 else -1
end

local function segment(c: canvas.Canvas, p: Placement, t1: number, s1: number, t2: number, s2: number)
  local x1, y1 = at(p, t1, s1)
  local x2, y2 = at(p, t2, s2)
  canvas.line(c, x1, y1, x2, y2, WIRE)
end

-- Points given as (t, s) pairs, joined.
local function shape(c: canvas.Canvas, p: Placement, local_: { number }, closed: boolean)
  local points: { number } = {}
  for i = 1, #local_, 2 do
    local x, y = at(p, local_[i], local_[i + 1])
    points[i], points[i + 1] = x, y
  end
  canvas.poly(c, points, closed, STROKE)
end

local function sign(c: canvas.Canvas, p: Placement, t: number, text: string)
  local x, y = at(p, t, 0)
  canvas.text(c, x, y + 4, text, "middle")
end

-- The jog from each pin to a symbol that is shifted off their line.
local function jogs(c: canvas.Canvas, p: Placement)
  if p.shift ~= 0 then
    local reach = p.len / 2
    local sx, sy = at(p, -reach, 0)
    local ex, ey = at(p, reach, 0)
    canvas.line(c, p.ax, p.ay, sx, sy, WIRE)
    canvas.line(c, p.bx, p.by, ex, ey, WIRE)
  end
end

-- The leads from the pins to the body.
local function leads(c: canvas.Canvas, p: Placement)
  local reach = p.len / 2
  jogs(c, p)
  if reach > p.half then
    segment(c, p, -reach, 0, -p.half, 0)
    segment(c, p, p.half, 0, reach, 0)
  end
end

local function resistor(c: canvas.Canvas, p: Placement)
  shape(c, p, { -18, 0, -15, -7, -9, 7, -3, -7, 3, 7, 9, -7, 15, 7, 18, 0 }, false)
end

local function capacitor(c: canvas.Canvas, p: Placement)
  segment(c, p, -4, -12, -4, 12)
  segment(c, p, 4, -12, 4, 12)
end

-- Four humps on the outward side.
local function inductor(c: canvas.Canvas, p: Placement)
  local side = outward(p) * 14
  local x, y = at(p, -24, 0)
  local pieces: { string | number } = { "M", x, y }
  for hump = 0, 3 do
    local t = -24 + hump * 12
    local cx, cy = at(p, t + 6, side)
    local ex, ey = at(p, t + 12, 0)
    table.insert(pieces, "Q")
    table.insert(pieces, cx)
    table.insert(pieces, cy)
    table.insert(pieces, ex)
    table.insert(pieces, ey)
  end
  canvas.path(c, pieces, STROKE)
end

-- `+` towards `to` and `−` towards `from`: the source raises `to`.
local function polarity(c: canvas.Canvas, p: Placement)
  sign(c, p, 8, "+")
  sign(c, p, -8, "−")
end

-- An arrow from `from` towards `to`: the way the source pushes its current.
local function arrow(c: canvas.Canvas, p: Placement)
  segment(c, p, -9, 0, 9, 0)
  shape(c, p, { 4, -4, 9, 0, 4, 4 }, false)
end

local function source(c: canvas.Canvas, p: Placement, voltage: boolean, dependent: boolean)
  if dependent then
    shape(c, p, { -17, 0, 0, -17, 17, 0, 0, 17 }, true)
  else
    canvas.circle(c, p.mx, p.my, 16, STROKE)
  end
  if voltage then
    polarity(c, p)
  else
    arrow(c, p)
  end
end

-- Two contacts and a blade: across them when closed, lifted when open.
local function switch(c: canvas.Canvas, p: Placement)
  local x1, y1 = at(p, -12, 0)
  local x2, y2 = at(p, 12, 0)
  canvas.circle(c, x1, y1, 2.5, DOT)
  canvas.circle(c, x2, y2, 2.5, DOT)
  if p.part.closed ~= false then
    segment(c, p, -12, 0, 12, 0)
  else
    segment(c, p, -12, 0, 9, outward(p) * 12)
  end
end

-- What a dependent source follows, as its caption and its control mark name it.
function symbols.controlLabel(part: model.Part, byName: { [string]: model.Part }): string
  local control = part.control
  local spec = model.DEPENDENT[part.type]
  if not control or not spec then
    return "?"
  elseif control.label then
    return control.label
  end
  local letter = if spec.by == "V" then "v" else "i"
  local target = if control.part then byName[control.part] else nil
  return letter .. "_{" .. (if target then canvas.plain(model.nameOf(target)) else "x") .. "}"
end

-- The lines under a part's name: its value, or for a dependent source its law.
local function valueText(part: model.Part, ac: boolean, byName: { [string]: model.Part }): string?
  local kind = part.type
  local spec = model.DEPENDENT[kind]
  if spec then
    local gain = part.gain
    if type(gain) ~= "number" then
      return nil
    end
    return format.eng(gain, spec.unit) .. "·" .. symbols.controlLabel(part, byName)
  end
  local unit = model.UNITS[kind]
  if not unit then
    return nil
  end
  local phasor = part.ac
  if ac and model.isSource(kind) and phasor then
    local magnitude = tonumber(phasor.magnitude) or tonumber(part.value)
    local phase = tonumber(phasor.phaseDeg) or 0
    if magnitude then
      local text = format.eng(magnitude, unit)
      return if phase ~= 0 then text .. " ∠ " .. format.number(phase) .. "°" else text
    end
  end
  local value = tonumber(part.value)
  return if value then format.eng(value, unit) else nil
end

-- Name and value, never over the symbol: on one line above a horizontal part
-- (below it when something is already there), on two lines to the right of
-- a vertical one (to its left likewise).
local function caption(c: canvas.Canvas, p: Placement, name: string?, value: string?)
  if p.horizontal then
    local text = if name and value then name .. " " .. value else name or value
    if text then
      canvas.place(c, {
        { x = p.mx, y = p.my - p.wide - 8, anchor = "middle" },
        { x = p.mx, y = p.my + p.wide + 17, anchor = "middle" },
      }, text)
    end
    return
  end
  local right, left = p.mx + p.wide + 8, p.mx - p.wide - 8
  if name and value then
    -- The longer line decides the side; the other follows it.
    local nameFirst = canvas.width(name) >= canvas.width(value)
    local first, firstY = if nameFirst then name else value, if nameFirst then p.my - 3 else p.my + 12
    local spot = canvas.place(c, {
      { x = right, y = firstY, anchor = "start" },
      { x = left, y = firstY, anchor = "end" },
    }, first)
    canvas.text(c, spot.x, if nameFirst then p.my + 12 else p.my - 3, if nameFirst then value else name, spot.anchor)
  elseif name or value then
    canvas.place(c, {
      { x = right, y = p.my + 4, anchor = "start" },
      { x = left, y = p.my + 4, anchor = "end" },
    }, (name or value) :: string)
  end
end

-- Draws a two-terminal part's leads and body, and marks the body as taken.
function symbols.draw(c: canvas.Canvas, p: Placement)
  local part = p.part
  local kind = part.type
  if kind == "wire" then
    jogs(c, p)
    segment(c, p, -p.len / 2, 0, p.len / 2, 0)
    return
  end
  leads(c, p)
  if kind == "R" then
    resistor(c, p)
  elseif kind == "C" then
    capacitor(c, p)
  elseif kind == "L" then
    inductor(c, p)
  elseif kind == "switch" then
    switch(c, p)
  else
    local spec = model.DEPENDENT[kind]
    source(c, p, if spec then spec.out == "V" else kind == "V", spec ~= nil)
  end
  -- Slanted strokes claim no room of their own, so the body does.
  local x1, y1 = at(p, -p.half, -p.wide)
  local x2, y2 = at(p, p.half, p.wide)
  canvas.occupy(c, x1, y1, x2, y2)
  x1, y1 = at(p, -p.half, p.wide)
  x2, y2 = at(p, p.half, -p.wide)
  canvas.occupy(c, x1, y1, x2, y2)
end

-- Captions a two-terminal part with its name and, unless `values` is false,
-- its value. Called once every body is drawn, so it can keep off them.
function symbols.caption(c: canvas.Canvas, p: Placement, ac: boolean, values: boolean, byName: { [string]: model.Part })
  local part = p.part
  local kind = part.type
  if kind == "wire" then
    return
  end
  -- A switch is captioned only when it was given a name.
  local name: string? = model.nameOf(part)
  if kind == "switch" and not part.label then
    name = nil
  end
  caption(c, p, name, if values then valueText(part, ac, byName) else nil)
end

-- A stem and three bars, always hanging below the point.
function symbols.ground(c: canvas.Canvas, a: Px)
  local x, y = a.x, a.y
  canvas.line(c, x, y, x, y + 12, WIRE)
  canvas.line(c, x - 12, y + 12, x + 12, y + 12, WIRE)
  canvas.line(c, x - 8, y + 18, x + 8, y + 18, WIRE)
  canvas.line(c, x - 4, y + 24, x + 4, y + 24, WIRE)
end

-- A filled dot: a junction, a probe or a named node.
function symbols.dot(c: canvas.Canvas, a: Px, radius: number, fill: string?)
  canvas.circle(c, a.x, a.y, radius, if fill then { fill = fill, stroke = "none" } else DOT)
end

-- An open terminal: a dot in a ring.
function symbols.terminal(c: canvas.Canvas, a: Px)
  canvas.circle(c, a.x, a.y, 2, DOT)
  canvas.circle(c, a.x, a.y, 5, STROKE)
end

--[[
  A triangle beside the inputs with the output at its apex. It points away
  from the inputs towards the output, and the input that is higher on the
  page gets the upper pin, so the leads never cross. Leads run vertically,
  then horizontally.
]]
function symbols.opamp(c: canvas.Canvas, part: model.Part, plus: Px, minus: Px, output: Px?)
  local right = output == nil or output.x >= math.max(plus.x, minus.x)
  local dir = if right then 1 else -1
  local edge = if right then math.max(plus.x, minus.x) + 20 else math.min(plus.x, minus.x) - 20
  local ym = (plus.y + minus.y) / 2
  local minusY = if minus.y <= plus.y then ym - 12 else ym + 12
  local plusY = 2 * ym - minusY
  local apex = edge + 40 * dir
  canvas.poly(c, { edge, ym - 24, edge, ym + 24, apex, ym }, true, STROKE)
  canvas.poly(c, { plus.x, plus.y, plus.x, plusY, edge, plusY }, false, LEAD)
  canvas.poly(c, { minus.x, minus.y, minus.x, minusY, edge, minusY }, false, LEAD)
  if output then
    canvas.poly(c, { apex, ym, output.x, ym, output.x, output.y }, false, LEAD)
  end
  canvas.text(c, edge + 8 * dir, minusY + 4, "−", "middle")
  canvas.text(c, edge + 8 * dir, plusY + 4, "+", "middle")
  canvas.text(c, edge + 20 * dir, ym - 30, model.nameOf(part), "middle")
end

return symbols
