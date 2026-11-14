--!strict
-- Greedy word wrap by character count. SVG has no text flow, so the card
-- breaks lines itself; widths are counted in UTF-8 characters, never bytes,
-- and a line is never cut inside a character.

local wrap = {}

local function chars(text: string): number
  return utf8.len(text) or #text
end

-- `word` split into pieces of at most `width` characters.
local function pieces(word: string, width: number): { string }
  local out: { string } = {}
  local current: { string } = {}
  for _, code in utf8.codes(word) do
    table.insert(current, utf8.char(code))
    if #current == width then
      table.insert(out, table.concat(current))
      current = {}
    end
  end
  if #current > 0 then
    table.insert(out, table.concat(current))
  end
  return out
end

-- The lines of `text`, each at most `width` characters. Runs of whitespace
-- (newlines included) collapse to one space; empty text gives no lines.
function wrap.lines(text: string, width: number): { string }
  local lines: { string } = {}
  local line = ""
  local function push(word: string)
    if line == "" then
      line = word
    elseif chars(line) + 1 + chars(word) <= width then
      line = line .. " " .. word
    else
      table.insert(lines, line)
      line = word
    end
  end
  for word in text:gmatch("%S+") do
    if chars(word) > width then
      for _, piece in pieces(word, width) do
        push(piece)
      end
    else
      push(word)
    end
  end
  if line ~= "" then
    table.insert(lines, line)
  end
  return lines
end

return wrap
