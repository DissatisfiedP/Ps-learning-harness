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

-- Engineering notation of a real number, three significant digits.
function format.eng(v: number, unit: string): string
  if math.abs(v) < 1e-12 then
    return "0 " .. unit
  end
  local sign = if v < 0 then "−" else ""
  local magnitude = math.abs(v)
  for _, p in PREFIXES do
    if magnitude >= p.scale then
      return sign .. string.format("%.3g", magnitude / p.scale) .. " " .. p.prefix .. unit
    end
  end
  -- Only NaN fails every comparison.
  return sign .. string.format("%.3g", magnitude) .. " " .. unit
end

-- A solved value: plain for real numbers, magnitude ∠ phase for phasors.
function format.value(value: num.Number, unit: string): string
  if type(value) == "table" then
    return format.eng(num.abs(value), unit) .. " ∠ " .. signed(string.format("%.3g", num.phaseDeg(value))) .. "°"
  end
  return format.eng(value, unit)
end

return format
