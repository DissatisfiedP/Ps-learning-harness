--!strict
-- Draws the card: the title, each premise with the objections that attack it
-- (and their replies) nested beneath, then the conclusion and the objections
-- to the inference. Kinds differ by outline as well as by heading, so the
-- card reads without colour: premises solid, the conclusion heavy, objections
-- dashed, replies dotted. A hidden part is a shaded box with "?".
local svg = require("@harness/svg")
local model = require("./model")
local wrap = require("./wrap")

local WIDTH = 560 -- px; the UI scales the figure down on narrow screens
local MARGIN = 16
local INDENT = 28 -- px per nesting level (objection, reply)
local PAD = 10
local GAP = 8 -- between boxes
local FONT = 13
local LINE = 18
local CHAR = 7.2 -- px per character at FONT, a generous average for system-ui
local TITLE_FONT = 15
local TITLE_LINE = 20
local TITLE_CHAR = 8.6
local FAMILY = "system-ui, sans-serif"

type Style = {
  outline: { [string]: string | number },
  label: string,
  labelSize: number?,
  heading: string?,
}

local card = {}

local function textAttrs(extra: { [string]: string | number }?): { [string]: string | number }
  local attrs: { [string]: string | number } = {
    fill = "currentColor",
    stroke = "none",
    ["font-family"] = FAMILY,
    ["font-size"] = FONT,
  }
  for k, v in extra or {} do
    attrs[k] = v
  end
  return attrs
end

local function labelWidth(label: string): number
  return math.max(36, math.ceil((utf8.len(label) or #label) * CHAR) + 10)
end

-- One box at nesting `level`, its top at `y`. Returns the box's bottom edge.
local function box(body: { string }, level: number, y: number, part: model.Part, style: Style): number
  local x = MARGIN + level * INDENT
  local w = WIDTH - MARGIN - x
  local labelW = labelWidth(style.label)
  local textX = x + PAD + labelW
  local columns = math.max(8, math.floor((w - 2 * PAD - labelW) / CHAR))

  -- Each line: its text and whether it is the bold heading.
  local lines: { { text: string, heading: boolean } } = {}
  if style.heading then
    for _, line in wrap.lines(style.heading, columns) do
      table.insert(lines, { text = line, heading = true })
    end
  end
  if part.hidden then
    table.insert(lines, { text = "? (fill in)", heading = false })
  else
    for _, line in wrap.lines(part.text or "", columns) do
      table.insert(lines, { text = line, heading = false })
    end
  end

  local h = 2 * PAD + (#lines - 1) * LINE + FONT + 3
  local outline = table.clone(style.outline)
  outline.rx = 4
  if part.hidden then
    outline.fill = "currentColor"
    outline["fill-opacity"] = "0.06"
  end
  table.insert(body, svg.rect(x, y, w, h, outline))
  local baseline = y + PAD + FONT
  local labelAttrs = textAttrs({ ["font-weight"] = 700, ["font-size"] = style.labelSize or FONT })
  table.insert(body, svg.text(x + PAD, baseline, style.label, labelAttrs))
  for i, line in lines do
    local attrs: { [string]: string | number } = {}
    if line.heading then
      attrs["font-weight"] = 600
    elseif part.hidden then
      attrs["font-style"] = "italic"
    end
    table.insert(body, svg.text(textX, baseline + (i - 1) * LINE, line.text, textAttrs(attrs)))
  end
  return y + h
end

-- The elbow from a parent box at `level - 1` (bottom edge `from`) to the left
-- edge of a child box at `level` whose top is `top`.
local function connector(body: { string }, level: number, from: number, top: number)
  local x = MARGIN + (level - 1) * INDENT + INDENT / 2
  local childX = MARGIN + level * INDENT
  local mid = top + PAD + FONT / 2
  table.insert(body, svg.path(string.format("M %g %g V %g H %g", x, from, mid, childX), { ["stroke-width"] = 1 }))
end

local function by(prefix: string, who: string?): string
  if who and who ~= "" then
    return prefix .. " — " .. who
  end
  return prefix
end

-- The objections to `target` and their replies, under a parent whose bottom
-- edge is `parentBottom`. Returns the new y.
local function objections(body: { string }, data: model.Card, target: string, parentBottom: number): number
  local y = parentBottom
  local targetName = if target == model.CONCLUSION then "the inference" else target
  for _, objection in model.attacking(data, target) do
    local top = y + GAP
    connector(body, 1, parentBottom, top)
    local bottom = box(body, 1, top, objection, {
      outline = { ["stroke-width"] = 1.5, ["stroke-dasharray"] = "6 4" },
      label = objection.id,
      heading = by("Objection to " .. targetName, objection.by),
    })
    y = bottom
    for _, reply in objection.replies or {} do
      local replyTop = y + GAP
      connector(body, 2, bottom, replyTop)
      y = box(body, 2, replyTop, reply, {
        outline = { ["stroke-width"] = 1.5, ["stroke-dasharray"] = "2 3" },
        label = reply.id,
        heading = by("Reply", reply.by),
      })
    end
  end
  return y
end

function card.draw(data: model.Card): string
  local body: { string } = {}
  local y = MARGIN
  local titleColumns = math.floor((WIDTH - 2 * MARGIN) / TITLE_CHAR)
  for i, line in wrap.lines(data.title or "", titleColumns) do
    local baseline = y + TITLE_FONT + (i - 1) * TITLE_LINE
    table.insert(
      body,
      svg.text(MARGIN, baseline, line, textAttrs({ ["font-size"] = TITLE_FONT, ["font-weight"] = 700 }))
    )
  end
  local titleLines = #wrap.lines(data.title or "", titleColumns)
  y += if titleLines > 0 then (titleLines - 1) * TITLE_LINE + TITLE_FONT + 2 * GAP else 0

  for i, premise in data.premises or {} do
    local top = if i == 1 then y else y + GAP
    local bottom = box(body, 0, top, premise, { outline = { ["stroke-width"] = 1.5 }, label = premise.id })
    y = objections(body, data, premise.id, bottom)
  end

  local conclusion: model.Part = data.conclusion or { id = model.CONCLUSION, hidden = true }
  local top = y + 2 * GAP
  table.insert(body, svg.line(MARGIN, top - GAP, WIDTH - MARGIN, top - GAP, { ["stroke-width"] = 1 }))
  local bottom =
    box(body, 0, top, conclusion, { outline = { ["stroke-width"] = 3 }, label = "∴", labelSize = 20 })
  y = objections(body, data, model.CONCLUSION, bottom)

  return svg.document(WIDTH, y + MARGIN, table.concat(body))
end

return card
