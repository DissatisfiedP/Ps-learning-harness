--!strict
--[[
  An SVG being drawn: the elements so far, the box they cover (so the
  document is cropped to its content whatever labels stick out), and the
  boxes that are taken, so a label can be put where nothing is drawn yet.

  Text understands subscripts: `R_1`, `V_out` and `V_{out 2}` draw the part
  after the underscore lowered and smaller.
]]
local svg = require("@harness/svg")

-- A taken rectangle: x1, y1, x2, y2.
type Box = { number }

export type Canvas = {
  out: { string },
  minX: number,
  minY: number,
  maxX: number,
  maxY: number,
  boxes: { Box },
}
export type Anchor = "start" | "middle" | "end"
export type TextStyle = { fill: string?, size: number?, bold: boolean? }

-- Where a label could go: its anchor point and how it is aligned to it.
export type Spot = { x: number, y: number, anchor: Anchor }

-- A label split into runs; `sub` runs are subscripts.
type Run = { text: string, sub: boolean }

local FONT = 12 -- px
local ADVANCE = 0.57 -- average glyph advance of a UI font, in em
local SUB_SCALE = 0.75
local SUB_DROP = 3 -- px a subscript sits below the baseline
local PAD = 14 -- px of space around the drawing

local canvas = {}

-- Two decimals are plenty for pixels, and keep the document short and stable.
local function r(v: number): number
  return math.floor(v * 100 + 0.5) / 100
end

function canvas.new(): Canvas
  return { out = {}, minX = math.huge, minY = math.huge, maxX = -math.huge, maxY = -math.huge, boxes = {} }
end

-- Grows the covered box to include (x, y) and `margin` around it.
function canvas.cover(c: Canvas, x: number, y: number, margin: number?)
  local m = margin or 0
  c.minX = math.min(c.minX, x - m)
  c.maxX = math.max(c.maxX, x + m)
  c.minY = math.min(c.minY, y - m)
  c.maxY = math.max(c.maxY, y + m)
end

-- Marks a rectangle as taken: labels placed later keep off it.
function canvas.occupy(c: Canvas, x1: number, y1: number, x2: number, y2: number)
  table.insert(c.boxes, { math.min(x1, x2), math.min(y1, y2), math.max(x1, x2), math.max(y1, y2) })
end

-- A stroke takes its place when it runs along an axis; a slanted one would
-- claim the whole rectangle around it, so it claims nothing.
local function occupyStroke(c: Canvas, x1: number, y1: number, x2: number, y2: number)
  if math.abs(x1 - x2) < 0.5 or math.abs(y1 - y2) < 0.5 then
    canvas.occupy(c, x1 - 1, y1 - 1, x2 + 1, y2 + 1)
  end
end

-- The area of the rectangle that lies on taken boxes.
function canvas.overlap(c: Canvas, x1: number, y1: number, x2: number, y2: number): number
  local area = 0
  for _, box in c.boxes do
    local w = math.min(x2, box[3]) - math.max(x1, box[1])
    local h = math.min(y2, box[4]) - math.max(y1, box[2])
    if w > 0 and h > 0 then
      area += w * h
    end
  end
  return area
end

function canvas.line(c: Canvas, x1: number, y1: number, x2: number, y2: number, attrs: svg.Attributes)
  canvas.cover(c, x1, y1, 1)
  canvas.cover(c, x2, y2, 1)
  occupyStroke(c, x1, y1, x2, y2)
  table.insert(c.out, svg.line(r(x1), r(y1), r(x2), r(y2), attrs))
end

-- A polyline (or, `closed`, a polygon) through `points`, a flat x, y, x, y list.
function canvas.poly(c: Canvas, points: { number }, closed: boolean, attrs: svg.Attributes)
  local d = table.create(#points / 2 + 1)
  for i = 1, #points, 2 do
    canvas.cover(c, points[i], points[i + 1], 1)
    if i > 1 then
      occupyStroke(c, points[i - 2], points[i - 1], points[i], points[i + 1])
    end
    table.insert(d, string.format("%s %g %g", if i == 1 then "M" else "L", r(points[i]), r(points[i + 1])))
  end
  if closed then
    table.insert(d, "Z")
  end
  table.insert(c.out, svg.path(table.concat(d, " "), attrs))
end

-- A path given as (command, x, y, …) pieces: numbers are rounded and every
-- coordinate pair grows the covered box.
function canvas.path(c: Canvas, pieces: { string | number }, attrs: svg.Attributes)
  local d = table.create(#pieces)
  local x: number? = nil
  for _, piece in pieces do
    if type(piece) == "number" then
      table.insert(d, string.format("%g", r(piece)))
      if x then
        canvas.cover(c, x, piece, 1)
        x = nil
      else
        x = piece
      end
    else
      table.insert(d, piece)
      x = nil
    end
  end
  table.insert(c.out, svg.path(table.concat(d, " "), attrs))
end

function canvas.circle(c: Canvas, x: number, y: number, radius: number, attrs: svg.Attributes)
  canvas.cover(c, x, y, radius + 1)
  canvas.occupy(c, x - radius, y - radius, x + radius, y + radius)
  table.insert(c.out, svg.circle(r(x), r(y), radius, attrs))
end

function canvas.rect(c: Canvas, x: number, y: number, w: number, h: number, attrs: svg.Attributes)
  canvas.cover(c, x, y, 1)
  canvas.cover(c, x + w, y + h, 1)
  table.insert(c.out, svg.rect(r(x), r(y), r(w), r(h), attrs))
end

-- Splits `value` at `_x`, `_word` and `_{any text}` subscripts.
local function runs(value: string): { Run }
  local out: { Run } = {}
  local function push(text: string, sub: boolean)
    if text ~= "" then
      table.insert(out, { text = text, sub = sub })
    end
  end
  local at = 1
  while true do
    local from = value:find("_", at, true)
    if not from then
      break
    end
    local braced = value:match("^{([^{}]*)}", from + 1)
    local word = if braced then nil else value:match("^[%w]+", from + 1)
    if from > 1 and (braced or word) then
      push(value:sub(at, from - 1), false)
      push((braced or word) :: string, true)
      at = from + 1 + (if braced then #braced + 2 else #(word :: string))
    else
      -- A lone underscore is just an underscore.
      push(value:sub(at, from), false)
      at = from + 1
    end
  end
  push(value:sub(at), false)
  return out
end

-- The label without its subscript markup.
function canvas.plain(value: string): string
  local parts = {}
  for _, run in runs(value) do
    table.insert(parts, run.text)
  end
  return table.concat(parts)
end

-- The drawn width of `value`, estimated: fonts differ between devices.
function canvas.width(value: string, size: number?): number
  local em = size or FONT
  local width = 0
  for _, run in runs(value) do
    local glyphs = utf8.len(run.text) or #run.text
    width += glyphs * em * ADVANCE * (if run.sub then SUB_SCALE else 1)
  end
  return width
end

-- The rectangle `value` would take with its baseline at y.
local function textBox(x: number, y: number, value: string, anchor: Anchor, size: number): (number, number, number, number)
  local width = canvas.width(value, size)
  local left = if anchor == "start" then x elseif anchor == "middle" then x - width / 2 else x - width
  return left, y - size + 1, left + width, y + SUB_DROP
end

-- Text with its baseline at y. Stroke is off: the document strokes by default.
function canvas.text(c: Canvas, x: number, y: number, value: string, anchor: Anchor, style: TextStyle?)
  local size = if style and style.size then style.size else FONT
  local attrs: svg.Attributes = {
    ["font-size"] = size,
    ["font-family"] = "system-ui, sans-serif",
    fill = if style and style.fill then style.fill else "currentColor",
    ["text-anchor"] = anchor,
    stroke = "none",
  }
  if style and style.bold then
    attrs["font-weight"] = "600"
  end

  local x1, y1, x2, y2 = textBox(x, y, value, anchor, size)
  canvas.cover(c, x1, y1 - 1)
  canvas.cover(c, x2, y2 + 1)
  canvas.occupy(c, x1, y1, x2, y2)

  local pieces = runs(value)
  if #pieces == 1 and not pieces[1].sub then
    table.insert(c.out, svg.text(r(x), r(y), value, attrs))
    return
  end
  -- Subscripts are tspans; `dy` moves the baseline down and back up. A space
  -- at a tspan's start is collapsed by some renderers, so it becomes a `dx`.
  local inner = {}
  local lowered = false
  for _, run in pieces do
    if run.sub then
      local shift = if lowered then "" else string.format(' dy="%d"', SUB_DROP)
      table.insert(inner, string.format('<tspan%s font-size="%g">%s</tspan>', shift, r(size * SUB_SCALE), svg.escape(run.text)))
      lowered = true
    elseif lowered then
      local rest = run.text:gsub("^ +", "")
      local gap = #run.text - #rest
      local dx = if gap > 0 then string.format(' dx="%g"', r(gap * size * 0.3)) else ""
      table.insert(inner, string.format('<tspan%s dy="%d">%s</tspan>', dx, -SUB_DROP, svg.escape(rest)))
      lowered = false
    else
      table.insert(inner, svg.escape(run.text))
    end
  end
  -- An empty `svg.text` is self-closed; reopen it around the runs.
  local open = svg.text(r(x), r(y), "", attrs)
  table.insert(c.out, open:sub(1, -3) .. ">" .. table.concat(inner) .. "</text>")
end

-- Draws `value` at the first of `spots` where it lies on nothing drawn so
-- far, or failing that at the one where it covers least. Returns that spot.
-- `padX` and `padY` ask for that much free room beside and above/below it,
-- for what the caller draws around the text.
function canvas.place(c: Canvas, spots: { Spot }, value: string, style: TextStyle?, padX: number?, padY: number?): Spot
  local size = if style and style.size then style.size else FONT
  local px, py = padX or 0, padY or 0
  local best, least = spots[1], math.huge
  for _, spot in spots do
    local x1, y1, x2, y2 = textBox(spot.x, spot.y, value, spot.anchor, size)
    -- A hair inside its own box, so text may touch what it sits next to.
    local covered = canvas.overlap(c, x1 + 1 - px, y1 + 1 - py, x2 - 1 + px, y2 - 1 + py)
    if covered < least then
      best, least = spot, covered
      if covered == 0 then
        break
      end
    end
  end
  canvas.text(c, best.x, best.y, value, best.anchor, style)
  return best
end

-- Draws through `draw` into a `<g>` with `attrs` (opacity, say). The layer
-- shares the canvas's taken boxes: it keeps off what is drawn, and what it
-- draws is kept off in turn.
function canvas.layer(c: Canvas, attrs: svg.Attributes, draw: (Canvas) -> ())
  local child = canvas.new()
  child.boxes = c.boxes
  draw(child)
  if #child.out == 0 then
    return
  end
  canvas.cover(c, child.minX, child.minY)
  canvas.cover(c, child.maxX, child.maxY)
  table.insert(c.out, svg.group(attrs, table.concat(child.out)))
end

-- The finished document, cropped to what was drawn plus padding.
function canvas.document(c: Canvas): string
  if #c.out == 0 then
    return svg.document(120, 60, "")
  end
  local dx, dy = math.ceil(PAD - c.minX), math.ceil(PAD - c.minY)
  local width = math.ceil(c.maxX - c.minX + 2 * PAD)
  local height = math.ceil(c.maxY - c.minY + 2 * PAD)
  local body = svg.group({ transform = string.format("translate(%d %d)", dx, dy) }, table.concat(c.out))
  return svg.document(width, height, body)
end

return canvas
