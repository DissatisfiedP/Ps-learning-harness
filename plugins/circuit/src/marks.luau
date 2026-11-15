--!strict
--[[
  What is drawn over a schematic to reason about it: current arrows and
  voltage polarities on parts, labels at points, current paths, mesh loops,
  crossed-out parts and highlighted regions. Marks are coloured so they stand
  apart from the circuit; the colours are mid-tones that read on a light and
  on a dark page.

  A label that has a choice of places goes to the first one where nothing is
  drawn yet (`canvas.place`), so the order of drawing matters: the circuit
  first, then marks at fixed places, then the labels that can move.
]]
local canvas = require("./canvas")
local symbols = require("./symbols")

type Canvas = canvas.Canvas
type Placement = symbols.Placement
type Spot = canvas.Spot

local COLORS: { [string]: string } = {
  red = "#e03131",
  blue = "#1c7ed6",
  green = "#2f9e44",
  orange = "#e8590c",
  purple = "#7048e8",
  gray = "#868e96",
}

local ROW = 15 -- px between two lines of labels

local marks = {}

marks.CURRENT = COLORS.blue
marks.VOLTAGE = COLORS.red
marks.NODE = COLORS.green
marks.REGION = COLORS.orange
marks.POWER = COLORS.purple

-- A named colour, or `fallback` for none or an unknown name.
function marks.color(name: string?, fallback: string): string
  return if name then COLORS[name] or fallback else fallback
end

-- A filled arrowhead with its tip at (x, y), pointing along (dx, dy).
local function head(c: Canvas, x: number, y: number, dx: number, dy: number, color: string, size: number)
  local bx, by = x - dx * size * 1.6, y - dy * size * 1.6
  local px, py = -dy * size * 0.7, dx * size * 0.7
  canvas.poly(c, { x, y, bx + px, by + py, bx - px, by - py }, true, { fill = color, stroke = "none" })
end

-- An arrowhead on the `from` lead of a part, pointing the way its current is
-- counted (from → to, or the other way when `reverse`), with `text` beside it.
function marks.current(c: Canvas, p: Placement, text: string?, reverse: boolean, color: string)
  local t = -(p.len / 2 + p.half) / 2
  local dir = if reverse then -1 else 1
  local x, y = symbols.at(p, t + 5 * dir, 0)
  head(c, x, y, p.ux * dir, p.uy * dir, color, 5.5)
  if not text then
    return
  end
  local mx, my = symbols.at(p, t, 0)
  local spots: { Spot }
  if p.horizontal then
    -- Starting at the pin and running towards the body, so it never crosses
    -- a wire that turns the corner there: under the lead, else over it.
    local leftToRight = p.ux >= 0
    local px = symbols.at(p, -p.len / 2, 0) + (if leftToRight then 8 else -8)
    local anchor: canvas.Anchor = if leftToRight then "start" else "end"
    spots = {
      { x = px, y = my + 19, anchor = anchor },
      { x = px, y = my - 9, anchor = anchor },
      { x = px, y = my + 19 + ROW, anchor = anchor },
    }
  else
    spots = {
      { x = mx - 9, y = my + 4, anchor = "end" },
      { x = mx + 9, y = my + 4, anchor = "start" },
    }
  end
  canvas.place(c, spots, text, { fill = color })
end

-- Appends a place a label could go. (A helper, so the anchor keeps its type.)
local function add(spots: { Spot }, x: number, y: number, anchor: canvas.Anchor)
  table.insert(spots, { x = x, y = y, anchor = anchor })
end

-- Where a reading that belongs to a part can go, nearest first: below a
-- horizontal part or left of a vertical one (the far side from its caption),
-- further out row by row, then on the caption's side beyond it.
local function beside(p: Placement): { Spot }
  local spots: { Spot } = {}
  if p.horizontal then
    for row = 0, 2 do
      add(spots, p.mx, p.my + p.wide + 15 + row * ROW, "middle")
    end
    for row = 1, 2 do
      add(spots, p.mx, p.my - p.wide - 8 - row * ROW, "middle")
    end
  else
    add(spots, p.mx - p.wide - 9, p.my + 4, "end")
    -- Beside the caption's two lines, above them and below them.
    for _, dy in { 4, -18, 27 } do
      add(spots, p.mx + p.wide + 8, p.my + dy, "start")
    end
    for step = 1, 4 do
      add(spots, p.mx + p.wide + 8 + step * 24, p.my + 4, "start")
    end
  end
  return spots
end

-- A reading that belongs to a part but has no direction (its power).
function marks.tag(c: Canvas, p: Placement, text: string, color: string)
  canvas.place(c, beside(p), text, { fill = color })
end

-- `+` towards the `from` end and `−` towards the `to` end of a part (swapped
-- when `reverse`), with `text` for the voltage V(from) − V(to). Beside a
-- horizontal part the signs flank the text and the three move together to a
-- free row. Beside a vertical part the signs stay by the ends of the body,
-- on the side away from the caption, and the text goes where there is room:
-- between them, beside a lead, or past the caption.
function marks.voltage(c: Canvas, p: Placement, text: string?, reverse: boolean, color: string)
  local style: canvas.TextStyle = { fill = color, bold = true }
  local fromSign, toSign = if reverse then "−" else "+", if reverse then "+" else "−"
  if p.horizontal then
    local spots: { Spot } = {}
    for row = 0, 2 do
      add(spots, p.mx, p.my + p.wide + 15 + row * ROW, "middle")
    end
    for row = 1, 2 do
      add(spots, p.mx, p.my - p.wide - 8 - row * ROW, "middle")
    end
    -- With no text the signs stand by the ends of the body.
    local half = math.min(p.len / 2 - 4, p.half + 10)
    local spot = spots[1]
    if text then
      half = canvas.width(text) / 2 + 10
      spot = canvas.place(c, spots, text, { fill = color }, 18, 0)
    end
    local fromSide = if p.ux >= 0 then -1 else 1
    canvas.text(c, p.mx + fromSide * half, spot.y, fromSign, "middle", style)
    canvas.text(c, p.mx - fromSide * half, spot.y, toSign, "middle", style)
    return
  end

  local left = p.mx - p.wide - 9
  local reach = p.half + 4
  local fromSide = if p.uy >= 0 then -1 else 1
  canvas.text(c, left + 3, p.my + fromSide * reach + 4, fromSign, "end", style)
  canvas.text(c, left + 3, p.my - fromSide * reach + 4, toSign, "end", style)
  if not text then
    return
  end
  -- Halfway along a lead, between the body and the pin.
  local lead = (p.half + p.len / 2) / 2
  local spots: { Spot } = {}
  add(spots, left, p.my + 4, "end")
  add(spots, p.mx + 9, p.my + lead + 4, "start")
  add(spots, p.mx - 9, p.my + lead + 4, "end")
  add(spots, p.mx + 9, p.my - lead + 4, "start")
  for step = 1, 4 do
    add(spots, p.mx + p.wide + 8 + step * 24, p.my + 4, "start")
  end
  canvas.place(c, spots, text, { fill = color })
end

-- `+` at one point, `−` at another and `text` halfway: a voltage between two
-- points that no single part spans. The marks sit beside the line between the
-- points (right of a vertical one, below a horizontal one), clear of
-- whatever is drawn on it.
function marks.between(c: Canvas, ax: number, ay: number, bx: number, by: number, text: string?, color: string)
  local len = math.max(1, math.sqrt((bx - ax) ^ 2 + (by - ay) ^ 2))
  local ux, uy = (bx - ax) / len, (by - ay) / len
  local inset = math.min(16, len / 4)
  local style: canvas.TextStyle = { fill = color, bold = true }
  if math.abs(uy) > math.abs(ux) then
    local x = math.max(ax, bx) + 12
    canvas.text(c, x, ay + uy * inset + 4, "+", "start", style)
    canvas.text(c, x, by - uy * inset + 4, "−", "start", style)
    if text then
      canvas.text(c, x, (ay + by) / 2 + 4, text, "start", { fill = color })
    end
  else
    local y = math.max(ay, by) + 17
    canvas.text(c, ax + ux * inset, y, "+", "middle", style)
    canvas.text(c, bx - ux * inset, y, "−", "middle", style)
    if text then
      canvas.text(c, (ax + bx) / 2, y, text, "middle", { fill = color })
    end
  end
end

-- An X over a part: it is out of the circuit being analysed.
function marks.cross(c: Canvas, p: Placement, color: string)
  local t, s = p.half + 8, math.max(p.wide, 8) + 5
  local attrs = { stroke = color, ["stroke-width"] = 2.5, ["stroke-linecap"] = "round" }
  local x1, y1 = symbols.at(p, -t, -s)
  local x2, y2 = symbols.at(p, t, s)
  canvas.line(c, x1, y1, x2, y2, attrs)
  x1, y1 = symbols.at(p, -t, s)
  x2, y2 = symbols.at(p, t, -s)
  canvas.line(c, x1, y1, x2, y2, attrs)
end

-- An X across a whole region.
function marks.crossBox(c: Canvas, x1: number, y1: number, x2: number, y2: number, color: string)
  local attrs = { stroke = color, ["stroke-width"] = 2.5, ["stroke-linecap"] = "round" }
  canvas.line(c, x1, y1, x2, y2, attrs)
  canvas.line(c, x1, y2, x2, y1, attrs)
end

-- A dashed outline around a region: a supernode, the part of the circuit
-- being replaced, and so on. Returns where its label can go: above a top
-- corner or below a bottom one.
function marks.highlight(c: Canvas, x1: number, y1: number, x2: number, y2: number, color: string): { Spot }
  canvas.rect(c, x1, y1, x2 - x1, y2 - y1, {
    stroke = color,
    ["stroke-width"] = 1.5,
    ["stroke-dasharray"] = "6 4",
    fill = color,
    ["fill-opacity"] = 0.07,
    rx = 10,
  })
  return {
    { x = x1 + 4, y = y1 - 6, anchor = "start" },
    { x = x2 - 4, y = y1 - 6, anchor = "end" },
    { x = x1 + 4, y = y2 + 15, anchor = "start" },
    { x = x2 - 4, y = y2 + 15, anchor = "end" },
    { x = x1 + 4, y = y1 - 6 - ROW, anchor = "start" },
  }
end

-- A translucent band along `points` (a flat x, y list) with an arrowhead on
-- each stretch: the path a current takes. Drawn under the circuit. Returns
-- where its label can go: beside the arrowhead of each stretch.
function marks.path(c: Canvas, points: { number }, closed: boolean, color: string): { Spot }
  local spots: { Spot } = {}
  if #points < 4 then
    return spots
  end
  canvas.poly(c, points, closed, {
    stroke = color,
    ["stroke-width"] = 7,
    ["stroke-opacity"] = 0.3,
    ["stroke-linejoin"] = "round",
    ["stroke-linecap"] = "round",
  })
  local count = #points / 2
  for i = 1, if closed then count else count - 1 do
    local j = i % count + 1
    local ax, ay, bx, by = points[2 * i - 1], points[2 * i], points[2 * j - 1], points[2 * j]
    local len = math.sqrt((bx - ax) ^ 2 + (by - ay) ^ 2)
    if len >= 24 then
      local ux, uy = (bx - ax) / len, (by - ay) / len
      -- A quarter of the way along: clear of a part drawn at the middle.
      local mx, my = ax + ux * len * 0.24, ay + uy * len * 0.24
      head(c, mx + ux * 6, my + uy * 6, ux, uy, color, 7)
      if math.abs(ux) >= math.abs(uy) then
        add(spots, mx, my + 21, "middle")
        add(spots, mx, my - 12, "middle")
      else
        add(spots, mx - 12, my + 4, "end")
        add(spots, mx + 12, my + 4, "start")
      end
    end
  end
  return spots
end

-- A mesh current: most of a circle with an arrowhead, `text` in its middle.
function marks.loop(c: Canvas, x: number, y: number, text: string?, clockwise: boolean, color: string)
  local radius = 17
  -- Screen angles (y points down), so a growing angle turns clockwise.
  local turn = if clockwise then 1 else -1
  local from, to = math.rad(110) * turn, math.rad(110 + 290) * turn
  local sx, sy = x + radius * math.cos(from), y + radius * math.sin(from)
  local ex, ey = x + radius * math.cos(to), y + radius * math.sin(to)
  canvas.cover(c, x, y, radius + 3)
  canvas.occupy(c, x - radius - 2, y - radius - 2, x + radius + 2, y + radius + 2)
  canvas.path(
    c,
    { "M", sx, sy, string.format("A %d %d 0 1 %d", radius, radius, if clockwise then 1 else 0), ex, ey },
    { stroke = color, ["stroke-width"] = 1.5, ["stroke-linecap"] = "round" }
  )
  head(c, ex, ey, -math.sin(to) * turn, math.cos(to) * turn, color, 5.5)
  if text then
    canvas.text(c, x, y + 4, text, "middle", { fill = color })
  end
end

function marks.note(c: Canvas, x: number, y: number, text: string, color: string?)
  canvas.text(c, x, y + 4, text, "middle", { fill = color })
end

-- `text` next to the point (x, y), wherever nothing is drawn: beside it,
-- above or below it, at a corner, and failing all those stacked above. A
-- probe and a node letter at one point therefore never overprint.
function marks.label(c: Canvas, x: number, y: number, text: string, color: string?)
  local spots: { Spot } = {
    { x = x + 9, y = y + 4, anchor = "start" },
    { x = x - 9, y = y + 4, anchor = "end" },
    { x = x, y = y - 9, anchor = "middle" },
    { x = x, y = y + 19, anchor = "middle" },
    { x = x + 7, y = y - 7, anchor = "start" },
    { x = x - 7, y = y - 7, anchor = "end" },
    { x = x + 7, y = y + 17, anchor = "start" },
    { x = x - 7, y = y + 17, anchor = "end" },
  }
  for level = 1, 3 do
    add(spots, x, y - 9 - level * ROW, "middle")
    add(spots, x + 7, y - 7 - level * ROW, "start")
  end
  canvas.place(c, spots, text, { fill = color })
end

return marks
