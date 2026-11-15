--!strict
-- The number domain of an analysis: real numbers for DC, phasors at one
-- angular frequency for AC. Source values and admittances in that domain.
local num = require("@harness/num")
local model = require("./model")

export type Domain = {
  ac: boolean,
  omega: number, -- rad/s; 0 for DC
  frequencyHz: number,
  zero: num.Number,
  one: num.Number,
  -- S = powerScale · V · I*: 1 for DC and RMS phasors, ½ for peak phasors.
  powerScale: number,
}

local phasor = {}

function phasor.domain(analysis: model.Analysis): Domain
  local ac = analysis.ac
  if ac then
    local frequency = tonumber(ac.frequencyHz) or 0
    return {
      ac = true,
      omega = 2 * math.pi * frequency,
      frequencyHz = frequency,
      zero = { re = 0, im = 0 },
      one = { re = 1, im = 0 },
      powerScale = if ac.rms then 1 else 0.5,
    }
  end
  return { ac = false, omega = 0, frequencyHz = 0, zero = 0, one = 1, powerScale = 1 }
end

-- A V or I source: its `value` in DC; in AC its `ac` phasor, whose magnitude
-- defaults to `value`. A source switched `off` is 0.
function phasor.source(part: model.Part, domain: Domain): num.Number
  if part.off then
    return domain.zero
  end
  if not domain.ac then
    return tonumber(part.value) or 0
  end
  local ac = part.ac
  local magnitude = tonumber(ac and ac.magnitude) or tonumber(part.value) or 0
  local phase = math.rad(tonumber(ac and ac.phaseDeg) or 0)
  return { re = magnitude * math.cos(phase), im = magnitude * math.sin(phase) }
end

-- Admittance of an R, a C or an AC inductor with a positive `value`. A DC
-- capacitor is open (0); a DC inductor has none here, as the solver treats
-- it as a 0 V source.
function phasor.admittance(kind: string, value: number, domain: Domain): num.Number
  if kind == "R" then
    return 1 / value
  elseif kind == "C" then
    return if domain.ac then { re = 0, im = domain.omega * value } else 0
  end
  return num.div(1, { re = 0, im = domain.omega * value })
end

return phasor
