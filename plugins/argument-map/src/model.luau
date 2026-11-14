--!strict
-- The request as the model sends it, its checks, and the card: the
-- learner-safe copy stored as the artifact's data (hidden parts lose their
-- text) and drawn by `card.luau`.

export type Reply = { by: string?, text: string }
export type Objection = {
  id: string,
  targets: string,
  by: string?,
  text: string,
  replies: { Reply }?,
}
export type Premise = { id: string, text: string }
export type Request = {
  title: string,
  conclusion: string,
  premises: { Premise },
  objections: { Objection }?,
  blanks: { string }?,
}

-- A drawn part. `text` is nil exactly when `hidden` is true.
export type Part = { id: string, by: string?, text: string?, hidden: boolean? }
export type CardObjection = Part & { targets: string, replies: { Part } }
export type Card = {
  title: string,
  conclusion: Part,
  premises: { Part },
  objections: { CardObjection },
}

local CONCLUSION = "conclusion"

local model = {}

model.CONCLUSION = CONCLUSION

local function nonEmpty(value: any, what: string): string
  if type(value) ~= "string" or value:match("^%s*$") then
    error(what .. " needs non-empty text", 0)
  end
  return value
end

-- The id a reply is addressed by in `blanks`.
function model.replyId(objectionId: string, index: number): string
  return objectionId .. "." .. tostring(index)
end

-- Checks the request and builds the card. Errors name the offending id so the
-- model can fix its call.
function model.card(request: Request): Card
  local title = nonEmpty(request.title, "`title`")
  local premises = request.premises or {}
  if #premises == 0 then
    error("an argument needs at least one premise", 0)
  end

  local seen: { [string]: string } = { [CONCLUSION] = "the conclusion" }
  local function claim(id: any, what: string): string
    if type(id) ~= "string" or id == "" then
      error(what .. " needs an `id`", 0)
    end
    if seen[id] then
      error(string.format("id `%s` is used twice (%s and %s)", id, seen[id], what), 0)
    end
    seen[id] = what
    return id
  end

  local blanks: { [string]: boolean } = {}
  for _, id in request.blanks or {} do
    blanks[id] = true
  end
  local used: { [string]: boolean } = {}
  local function part(id: string, by: string?, text: string): Part
    local hidden = blanks[id] == true
    if hidden then
      used[id] = true
    end
    return { id = id, by = by, text = if hidden then nil else text, hidden = if hidden then true else nil }
  end

  local isPremise: { [string]: boolean } = {}
  local cardPremises: { Part } = {}
  for i, premise in premises do
    local id = claim(premise.id, "premise " .. i)
    isPremise[id] = true
    table.insert(cardPremises, part(id, nil, nonEmpty(premise.text, "premise `" .. id .. "`")))
  end

  local cardObjections: { CardObjection } = {}
  for i, objection in request.objections or {} do
    local id = claim(objection.id, "objection " .. i)
    local targets = objection.targets
    if targets ~= CONCLUSION and not isPremise[targets] then
      error(
        string.format(
          "objection `%s` targets `%s`, which is not a premise id (or \"conclusion\")",
          id,
          tostring(targets)
        ),
        0
      )
    end
    local replies: { Part } = {}
    for r, reply in objection.replies or {} do
      local rid = claim(model.replyId(id, r), "reply " .. r .. " to `" .. id .. "`")
      table.insert(replies, part(rid, reply.by, nonEmpty(reply.text, "reply `" .. rid .. "`")))
    end
    local drawn = part(id, objection.by, nonEmpty(objection.text, "objection `" .. id .. "`"))
    table.insert(cardObjections, {
      id = drawn.id,
      by = drawn.by,
      text = drawn.text,
      hidden = drawn.hidden,
      targets = targets,
      replies = replies,
    })
  end

  local conclusion = part(CONCLUSION, nil, nonEmpty(request.conclusion, "`conclusion`"))

  for _, id in request.blanks or {} do
    if not used[id] then
      error(
        string.format(
          "blank `%s` is not a premise, objection or reply id (replies are `<objection id>.<n>`) or \"conclusion\"",
          id
        ),
        0
      )
    end
  end

  return { title = title, conclusion = conclusion, premises = cardPremises, objections = cardObjections }
end

-- The objections that attack `target`, in request order.
function model.attacking(card: Card, target: string): { CardObjection }
  local out: { CardObjection } = {}
  for _, objection in card.objections or {} do
    if objection.targets == target then
      table.insert(out, objection)
    end
  end
  return out
end

return model
