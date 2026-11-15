--!strict
-- Values as people read them: "4.7 kΩ", "−10 V", "707 mV ∠ −45°".
local num = require("@harness/num")

local PREFIXES = {
  { scale = 1e9, prefix = "G" },
  { scale = 1e6, prefix = "M" },
  { scale = 1e3, prefix = "k" },
  { scale = 1, prefix = "" },
  { scale = 1e-3, prefix = "m" },
  { scale = 1e-6, prefix = "µ" },
  { scale = 1e-9, prefix = "n" },
  { scale = 1e-12, prefix = "p" },
}

local format = {}

-- A typographic minus instead of a leading hyphen.
local function signed(text: string): string
  return (text:gsub("^%-", "−"))
end

-- A plain number, three significant digits: "2", "−0.5", "1e+06".
function format.number(v: number): string
  return signed(string.format("%.3g", v))
end

-- Engineering notation of a real number, three significant digits. An empty
-- unit gives a bare number ("2.5 k" would read as a unit, so no prefix then).
function format.eng(v: number, unit: string): string
  if unit == "" then
    return format.number(if math.abs(v) < 1e-12 then 0 else v)
  end
  if math.abs(v) < 1e-12 then
    return "0 " .. unit
  end
  local sign = if v < 0 then "−" else ""
  -- Round first, so 999.99 µA reads 1 mA rather than 1e+03 µA.
  local magnitude = tonumber(string.format("%.3g", math.abs(v))) or math.abs(v)
  for _, p in PREFIXES do
    if magnitude >= p.scale then
      return sign .. string.format("%.3g", magnitude / p.scale) .. " " .. p.prefix .. unit
    end
  end
  -- Smaller than a pico-unit, or NaN (which fails every comparison).
  return sign .. string.format("%.3g", magnitude) .. " " .. unit
end

-- A solved value: plain for real numbers, magnitude ∠ phase for phasors.
function format.value(value: num.Number, unit: string): string
  if type(value) == "table" then
    return format.eng(num.abs(value), unit) .. " ∠ " .. format.number(num.phaseDeg(value)) .. "°"
  end
  return format.eng(value, unit)
end

-- A point key "x,y" as "(x,y)".
function format.point(key: string): string
  return "(" .. key .. ")"
end

return format
