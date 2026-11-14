--!strict
-- The text the model reads after `argument_map_show`: what the card holds,
-- which objection attacks what, and the text of every hidden part (the card
-- and the stored data never carry it, so this is how the tutor grades).
local model = require("./model")

local summary = {}

local function count(n: number, one: string, many: string): string
  return string.format("%d %s", n, if n == 1 then one else many)
end

function summary.text(request: model.Request, data: model.Card): string
  -- Every part's full text by id, from the request.
  local full: { [string]: string } = { [model.CONCLUSION] = request.conclusion }
  for _, premise in request.premises do
    full[premise.id] = premise.text
  end
  local replies = 0
  local attacks: { string } = {}
  for _, objection in request.objections or {} do
    full[objection.id] = objection.text
    table.insert(attacks, objection.id .. " → " .. objection.targets)
    for r, reply in objection.replies or {} do
      full[model.replyId(objection.id, r)] = reply.text
      replies += 1
    end
  end

  local out = {
    string.format(
      "Argument map “%s”: %s, a conclusion, %s, %s.",
      data.title,
      count(#data.premises, "premise", "premises"),
      count(#data.objections, "objection", "objections"),
      count(replies, "reply", "replies")
    ),
  }
  if #attacks > 0 then
    table.insert(out, "Attacks: " .. table.concat(attacks, ", ") .. ".")
  end

  local hidden: { string } = {}
  local listed: { [string]: boolean } = {}
  for _, id in request.blanks or {} do
    if not listed[id] then
      listed[id] = true
      table.insert(hidden, string.format("%s = “%s”", id, full[id]))
    end
  end
  if #hidden > 0 then
    table.insert(out, "Hidden from the learner (shown as ?): " .. table.concat(hidden, "; ") .. ".")
  else
    table.insert(out, "Nothing is hidden.")
  end
  return table.concat(out, "\n")
end

return summary
