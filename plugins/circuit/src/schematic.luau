--!strict
-- Lays the parts out on a pixel grid and draws the whole schematic.
local svg = require("@harness/svg")
local num = require("@harness/num")
local model = require("./model")
local symbols = require("./symbols")

local GRID = 70 -- px per grid step
local MARGIN = 50 -- px around the outermost points

local schematic = {}

-- `probes` holds the values the probe labels show; nil shows none.
function schematic.draw(parts: { model.Part }, probes: { [string]: num.Number }?): string
  local minX, minY, maxX, maxY = 0, 0, 1, 1
  for _, part in parts do
    for _, pin in model.pins(part, true) do
      local cell = model.cell(pin)
      if cell then
        minX = math.min(minX, cell.x)
        maxX = math.max(maxX, cell.x)
        minY = math.min(minY, cell.y)
        maxY = math.max(maxY, cell.y)
      end
    end
  end
  local function px(cell: model.Cell): symbols.Px
    return { x = MARGIN + (cell.x - minX) * GRID, y = MARGIN + (cell.y - minY) * GRID }
  end

  local body: { string } = {}
  for _, part in parts do
    local from = model.cell(part.from)
    if from then
      local a = px(from)
      local to = model.cell(part.to)
      local onePoint = part.type == "ground" or part.type == "probe" or not to
      local b = if to and not onePoint then px(to) else a
      local third = model.cell(part.third)
      symbols.draw(body, part, a, b, if third then px(third) else nil, probes)
    end
  end
  local width = MARGIN * 2 + (maxX - minX) * GRID + 80
  local height = MARGIN * 2 + (maxY - minY) * GRID + 60
  return svg.document(width, height, table.concat(body))
end

return schematic
