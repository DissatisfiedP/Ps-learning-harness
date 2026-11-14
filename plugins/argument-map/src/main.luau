--!strict
-- Entry module: the functions plugin.toml names. Tool `argument_map_show`
-- (entry `show`) and the renderer `render`.
local model = require("./model")
local card = require("./card")
local summary = require("./summary")

local function show(request: model.Request)
  local data = model.card(request)
  return {
    text = summary.text(request, data),
    artifact = {
      kind = "svg",
      svg = card.draw(data),
      data = data,
    },
  }
end

-- Redraws a stored card. The data is the learner-safe copy, so a hidden part
-- stays hidden.
local function render(data: model.Card): string
  return card.draw(data)
end

return {
  show = show,
  render = render,
}
