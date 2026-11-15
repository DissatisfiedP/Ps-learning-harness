-- Circuit fixtures. Run with `cargo run -p harness --bin plugin -- test plugins/circuit`.
-- Grid y grows upwards: (0, 2) is above (0, 0).
local api = require("../src/main")
local num = require("@harness/num")
local test = require("@harness/test")

local function close(got, want: number, tol: number?)
  local tolerance = tol or 1e-6
  assert(type(got) == "number", "expected a real number, got " .. tostring(got))
  assert(math.abs(got - want) <= tolerance, string.format("expected %.9g, got %.9g", want, got))
end

local function warnings(result): string
  return table.concat(result.warnings or {}, "; ")
end

local function has(text: string, wanted: string)
  assert(text:find(wanted, 1, true), "missing `" .. wanted .. "` in:\n" .. text)
end

local function lacks(text: string, unwanted: string)
  assert(not text:find(unwanted, 1, true), "unexpected `" .. unwanted .. "` in:\n" .. text)
end

local function dc(components, extra)
  local request = { analysis = { dc = true }, components = components }
  for key, value in extra or {} do
    request[key] = value
  end
  return request
end

local GND = { id = "GND", type = "ground", from = { 0, 0 } }

local divider = dc({
  { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 12, label = "Vin" },
  { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 3000, label = "R1" },
  { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000, label = "R2" },
  { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
  GND,
  { id = "P1", type = "probe", from = { 2, 2 }, to = { 2, 0 }, label = "Vout" },
})

-- The divider with one more part appended.
local function dividerWith(part, extra)
  local components = table.clone(divider.components)
  table.insert(components, part)
  return dc(components, extra)
end

local amp = dc({
  { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
  { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000 },
  { id = "R2", type = "R", from = { 2, 2 }, to = { 4, 2 }, value = 10000 },
  { id = "U1", type = "opamp", from = { 2, 0 }, to = { 2, 2 }, third = { 4, 2 } },
  { id = "W1", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
  GND,
  { id = "P1", type = "probe", from = { 4, 2 }, label = "Vout" },
})

local R_LP, C_LP = 1000, 1e-6
local function lowpass(rms: boolean?)
  return {
    analysis = { ac = { frequencyHz = 1 / (2 * math.pi * R_LP * C_LP), rms = rms } },
    components = {
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, ac = { magnitude = 1, phaseDeg = 0 } },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = R_LP },
      { id = "C1", type = "C", from = { 2, 2 }, to = { 2, 0 }, value = C_LP },
      { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
      GND,
      { id = "P1", type = "probe", from = { 2, 2 }, to = { 2, 0 }, label = "Vout" },
      { id = "P2", type = "probe", part = "R1", quantity = "power", label = "P_R" },
    },
  }
end

-- A 12 V source and a 3 mA source feeding one node: 8 V + 2 V by superposition.
local twoSources = dc({
  { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 12, label = "V_1" },
  { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000, label = "R_1" },
  { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 2000, label = "R_2" },
  { id = "R3", type = "R", from = { 2, 2 }, to = { 4, 2 }, value = 1000, label = "R_3" },
  { id = "I1", type = "I", from = { 4, 0 }, to = { 4, 2 }, value = 0.003, label = "I_1" },
  { id = "W0", type = "wire", from = { 4, 0 }, to = { 0, 0 } },
  GND,
  { id = "P1", type = "probe", from = { 2, 2 }, label = "V_x" },
})

return {
  -- Solving -----------------------------------------------------------------

  divider = function()
    local out = api.solve(divider)
    close(out.probes.Vout, 3, 1e-6)
    assert(warnings(out) == "", warnings(out))
  end,

  loaded_divider = function()
    local loaded = dividerWith({ id = "RL", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 3000 })
    close(api.solve(loaded).probes.Vout, 2.4, 1e-6)
  end,

  rc_lowpass_cutoff = function()
    local out = api.solve(lowpass())
    close(num.abs(out.probes.Vout), 1 / math.sqrt(2), 1e-4)
    close(num.phaseDeg(out.probes.Vout), -45, 1e-3)
  end,

  rc_cutoff_text_is_a_phasor = function()
    has(api.circuit_solve(lowpass()).text, "Vout = 707 mV ∠ −45°")
    local drawn = api.circuit_draw(lowpass())
    has(drawn.text, "Vout = 707 mV ∠ −45°")
    has(drawn.artifact.svg, "Vout: 707 mV ∠ −45°")
    has(drawn.artifact.svg, "f = 159 Hz")
  end,

  ac_power_depends_on_peak_or_rms = function()
    -- |I| = 1/(√2 kΩ): ½|I|²R = 250 µW for a 1 V peak source, |I|²R = 500 µW for 1 V RMS.
    close(api.solve(lowpass()).probes.P_R, 250e-6, 1e-9)
    close(api.solve(lowpass(true)).probes.P_R, 500e-6, 1e-9)
    has(api.circuit_solve(lowpass()).text, "phasors are peak values")
    -- The capacitor absorbs no real power, only (negative) reactive power.
    has(api.circuit_solve(lowpass()).text, "C1 = 0 W (Q −250 µvar)")
  end,

  dc_text_reports_nodes_currents_and_powers = function()
    local text = api.circuit_solve(divider).text
    has(text, "Vout = 3 V")
    has(text, "node R1–R2 = 3 V")
    has(text, "node R1–Vin = 12 V")
    -- 12 V across 4 kΩ: 3 mA through R1 and R2; inside the source it flows − to +, i.e. from → to.
    has(text, "R1 = 3 mA")
    has(text, "R2 = 3 mA")
    has(text, "Vin = 3 mA")
    -- The source supplies what the resistors absorb.
    has(text, "R1 = 27 mW")
    has(text, "R2 = 9 mW")
    has(text, "Vin = −36 mW")
  end,

  dc_inductor_shorts_and_capacitor_opens = function()
    -- 10 V into L1 then R1 to ground, with C1 across R1: in DC the inductor
    -- drops nothing and the capacitor carries nothing.
    local text = api.circuit_solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 10 },
      { id = "L1", type = "L", from = { 0, 2 }, to = { 2, 2 }, value = 0.01 },
      { id = "R1", type = "R", from = { 2, 2 }, to = { 0, 0 }, value = 1000 },
      { id = "C1", type = "C", from = { 2, 2 }, to = { 0, 0 }, value = 1e-6 },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "Vr" },
    })).text
    has(text, "Vr = 10 V")
    has(text, "L1 = 10 mA")
    has(text, "C1 = 0 A")
  end,

  open_switch_does_not_join = function()
    local function vout(closed: boolean)
      return api.solve(dc({
        { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 10 },
        { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000 },
        { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000 },
        { id = "S1", type = "switch", from = { 2, 2 }, to = { 2, 0 }, closed = closed },
        { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
        GND,
        { id = "P", type = "probe", from = { 2, 2 }, label = "Vout" },
      })).probes.Vout
    end
    close(vout(false), 5, 1e-9)
    close(vout(true), 0, 1e-9)
  end,

  a_pin_on_a_wire_joins_it = function()
    -- R2 lands on the middle of the bottom wire: a T-junction, with no part ending there.
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 12 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 3000 },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000 },
      { id = "W0", type = "wire", from = { 0, 0 }, to = { 4, 0 } },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "Vout" },
    }))
    close(out.probes.Vout, 3, 1e-9)
    assert(warnings(out) == "", warnings(out))
  end,

  a_pin_on_a_part_body_warns = function()
    -- R2 ends at the middle of R1, which is not a connection.
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 12 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 4, 2 }, value = 1000 },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000 },
      { id = "R3", type = "R", from = { 4, 2 }, to = { 4, 0 }, value = 1000 },
      { id = "W0", type = "wire", from = { 0, 0 }, to = { 4, 0 } },
      GND,
    }))
    has(warnings(out), "on the body of `R1`")
  end,

  ground_ignores_to_and_probe_defaults_to_ground = function()
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 12 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 3000 },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000 },
      { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
      { id = "GND", type = "ground", from = { 0, 0 }, to = { 0, 1 } },
      { id = "P1", type = "probe", from = { 2, 2 }, label = "Vout" },
      { id = "P2", type = "probe", from = { 0, 2 }, to = { 2, 2 }, label = "VR1" },
    }))
    close(out.probes.Vout, 3, 1e-6)
    close(out.probes.VR1, 9, 1e-6)
    assert(warnings(out) == "", warnings(out))
  end,

  inverting_opamp = function()
    local out = api.solve(amp)
    close(out.probes.Vout, -10, 1e-9)
    assert(warnings(out) == "", warnings(out))
  end,

  opamp_with_tied_inputs_warns = function()
    local out = api.solve(dc({
      { id = "U1", type = "opamp", from = { 0, 0 }, to = { 0, 0 }, third = { 5, 5 } },
      GND,
    }))
    has(warnings(out), "both inputs on one node")
  end,

  -- Dependent sources ---------------------------------------------------------

  vcvs_follows_a_part_voltage = function()
    -- 1 V across R1; E1 puts 5 × that across RL.
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
      { id = "E1", type = "VCVS", from = { 2, 0 }, to = { 2, 2 }, gain = 5, control = { part = "R1" } },
      { id = "RL", type = "R", from = { 2, 2 }, to = { 4, 2 }, value = 1000 },
      { id = "W1", type = "wire", from = { 4, 2 }, to = { 4, 0 } },
      { id = "W0", type = "wire", from = { 4, 0 }, to = { 0, 0 } },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "Vout" },
    }))
    close(out.probes.Vout, 5, 1e-9)
    close(out.currents.RL, 0.005, 1e-12)
    assert(warnings(out) == "", warnings(out))
  end,

  vcvs_follows_two_points = function()
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 2 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
      { id = "E1", type = "VCVS", from = { 2, 0 }, to = { 2, 2 }, gain = -3, control = { plus = { 0, 2 }, minus = { 0, 0 } } },
      { id = "RL", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000 },
      { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "Vout" },
    }))
    close(out.probes.Vout, -6, 1e-9)
  end,

  vccs_in_a_node_equation = function()
    -- (VA − 10)/2 + VA/4 − 0.5·VA = 0, so VA = 20 V.
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 10 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 2 },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 4 },
      { id = "G1", type = "VCCS", from = { 4, 0 }, to = { 4, 2 }, gain = 0.5, control = { part = "R2" } },
      { id = "W1", type = "wire", from = { 2, 2 }, to = { 4, 2 } },
      { id = "W0", type = "wire", from = { 4, 0 }, to = { 0, 0 } },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "VA" },
    }))
    close(out.probes.VA, 20, 1e-9)
    close(out.currents.G1, 10, 1e-9)
  end,

  cccs_follows_a_part_current = function()
    -- 10 mA through R1; F1 pushes twice that into R2 = 100 Ω.
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 10 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
      { id = "F1", type = "CCCS", from = { 2, 0 }, to = { 2, 2 }, gain = 2, control = { part = "R1" } },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 4, 2 }, value = 100 },
      { id = "W1", type = "wire", from = { 4, 2 }, to = { 4, 0 } },
      { id = "W0", type = "wire", from = { 4, 0 }, to = { 0, 0 } },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "Vx" },
    }))
    close(out.probes.Vx, 2, 1e-9)
  end,

  ccvs_follows_a_wire_current = function()
    -- 2 A flows through the wire Wx; H1 makes 3 Ω × 2 A = 6 V.
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 10 },
      { id = "Wx", type = "wire", from = { 0, 2 }, to = { 2, 2 } },
      { id = "R1", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 5 },
      { id = "H1", type = "CCVS", from = { 4, 0 }, to = { 4, 2 }, gain = 3, control = { part = "Wx" } },
      { id = "RL", type = "R", from = { 4, 2 }, to = { 6, 2 }, value = 6 },
      { id = "W1", type = "wire", from = { 6, 2 }, to = { 6, 0 } },
      { id = "W0", type = "wire", from = { 6, 0 }, to = { 0, 0 } },
      GND,
      { id = "P", type = "probe", from = { 4, 2 }, label = "Vh" },
    }))
    close(out.probes.Vh, 6, 1e-9)
    close(out.currents.Wx, 2, 1e-9)
    assert(warnings(out) == "", warnings(out))
  end,

  a_dependent_source_without_its_control_is_left_out = function()
    local function solved(part)
      local components = table.clone(divider.components)
      table.insert(components, part)
      return warnings(api.solve(dc(components)))
    end
    has(solved({ id = "G1", type = "VCCS", from = { 2, 0 }, to = { 2, 2 }, gain = 1 }), "`G1` needs a `control`")
    has(solved({ id = "G1", type = "VCCS", from = { 2, 0 }, to = { 2, 2 }, control = { part = "R1" } }), "`G1` needs a numeric `gain`")
    has(solved({ id = "G1", type = "VCCS", from = { 2, 0 }, to = { 2, 2 }, gain = 1, control = { part = "nope" } }), "`nope`, which is not a part")
    has(solved({ id = "F1", type = "CCCS", from = { 2, 0 }, to = { 2, 2 }, gain = 1, control = { plus = { 0, 2 } } }), "the part whose current it follows")
  end,

  -- Probes on parts -----------------------------------------------------------

  probes_read_part_voltage_current_and_power = function()
    local components = table.clone(divider.components)
    table.insert(components, { id = "Pv", type = "probe", part = "R1", label = "v_1" })
    table.insert(components, { id = "Pi", type = "probe", part = "R2", quantity = "current", label = "i_2" })
    table.insert(components, { id = "Pp", type = "probe", part = "R2", quantity = "power", label = "p_2" })
    table.insert(components, { id = "Pw", type = "probe", part = "W0", quantity = "current", label = "i_w" })
    local request = dc(components)
    local out = api.solve(request)
    close(out.probes.v_1, 9, 1e-9)
    close(out.probes.i_2, 0.003, 1e-12)
    close(out.probes.p_2, 0.009, 1e-12)
    -- The wire carries R2's current back to the source: from (2,0) to (0,0).
    close(out.probes.i_w, 0.003, 1e-12)
    assert(out.units.i_2 == "A" and out.units.p_2 == "W" and out.units.v_1 == "V")
    local text = api.circuit_solve(request).text
    has(text, "i_2 = 3 mA")
    has(text, "p_2 = 9 mW")
    local svg = api.circuit_draw(request).artifact.svg
    has(svg, "3 mA")
    has(svg, "9 mW")
  end,

  a_probe_on_a_missing_part_warns = function()
    local out = api.solve(dividerWith({ id = "Px", type = "probe", part = "R9", label = "v_9" }))
    has(warnings(out), "probe `v_9` reads `R9`, which is not a part in the solution")
  end,

  -- Warnings --------------------------------------------------------------------

  floating_node_warns = function()
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
      GND,
      { id = "R1", type = "R", from = { 2, 2 }, to = { 4, 2 }, value = 1000 },
    }))
    has(warnings(out), "dangling")
  end,

  nodes_touched_by_the_same_parts_get_their_point = function()
    -- Both ends of the lone R1 are "node R1"; the point tells them apart.
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
      { id = "R0", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
      GND,
      { id = "R1", type = "R", from = { 2, 2 }, to = { 4, 2 }, value = 1000 },
    }))
    has(warnings(out), "`node R1 (2,2)` is dangling")
    has(warnings(out), "`node R1 (4,2)` is dangling")
  end,

  probe_does_not_hide_dangling_node = function()
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000 },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "Vend" },
    }))
    has(warnings(out), "dangling")
  end,

  a_terminal_marks_an_end_left_open_on_purpose = function()
    -- R3 ends at the terminal `a` and carries nothing: the open-circuit
    -- voltage there is the divider's, and nothing warns.
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 12 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 3000 },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000 },
      { id = "R3", type = "R", from = { 2, 2 }, to = { 4, 2 }, value = 500 },
      { id = "Ta", type = "terminal", from = { 4, 2 }, label = "a" },
      { id = "Tb", type = "terminal", from = { 4, 0 }, label = "b" },
      { id = "W0", type = "wire", from = { 4, 0 }, to = { 0, 0 } },
      GND,
      { id = "P", type = "probe", from = { 4, 2 }, to = { 4, 0 }, label = "v_oc" },
    }))
    close(out.probes.v_oc, 3, 1e-9)
    close(out.nodeNames.a, 3, 1e-9)
    assert(warnings(out) == "", warnings(out))
  end,

  a_node_behind_an_open_switch_floats = function()
    local out = api.solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 5 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000 },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000 },
      { id = "S1", type = "switch", from = { 2, 2 }, to = { 4, 2 }, closed = false },
      { id = "T", type = "terminal", from = { 4, 2 }, label = "a" },
      { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "Vout" },
    }))
    close(out.probes.Vout, 2.5, 1e-9)
    has(warnings(out), "`a` floats")
  end,

  non_positive_values_warn = function()
    for _, v in { 0, -100 } do
      local out = api.solve(dc({
        { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
        { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
        { id = "R0", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = v },
        GND,
      }))
      has(warnings(out), "`R0` needs a positive value")
    end
  end,

  unknown_types_and_missing_pins_warn = function()
    local out = api.solve(dividerWith({ id = "D1", type = "diode", from = { 2, 2 }, to = { 2, 0 } }))
    has(warnings(out), "`D1` has the unknown type `diode`")
    close(out.probes.Vout, 3, 1e-9)
    has(warnings(api.solve(dividerWith({ id = "R9", type = "R", from = { 2, 2 }, value = 10 }))), "`R9` needs both `from` and `to`")
  end,

  -- Hidden answers -----------------------------------------------------------

  hidden_probe_omits_every_value_that_gives_it_away = function()
    local hidden = table.clone(divider)
    hidden.components = table.clone(divider.components)
    hidden.components[6] = { id = "P1", type = "probe", from = { 2, 2 }, to = { 2, 0 }, label = "Vout", hidden = true }
    hidden.show = { nodes = true, voltages = true, currents = true }
    local drawn = api.circuit_draw(hidden)
    local svg, data = drawn.artifact.svg, drawn.artifact.data
    has(svg, "Vout: ?")
    lacks(svg, "3 V")
    lacks(svg, "3 mA")
    assert(data.result.probes.Vout == nil, "hidden probe value in artifact data")
    assert(next(data.result.nodes) == nil, "node voltages in artifact data")
    assert(next(data.result.currents) == nil, "currents in artifact data")
    local viewed = api.solve(hidden)
    assert(viewed.probes.Vout == nil and next(viewed.nodes) == nil, "hidden values in view solve")
    has(api.render(data), "Vout: ?")
    -- The model still gets everything, and is told the figure shows less.
    has(drawn.text, "Vout (hidden from the learner) = 3 V")
    has(drawn.text, "R1 = 3 mA")
    has(drawn.text, "are ignored while a probe is hidden")
  end,

  -- Analyses in the text ------------------------------------------------------

  series_and_parallel_groups = function()
    local text = api.circuit_solve(dividerWith({ id = "RL", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 3000 })).text
    has(text, "Series (same current): Vin – R1.")
    has(text, "Parallel (same voltage): R2 ∥ RL = 750 Ω.")
    -- The plain divider is one series loop.
    has(api.circuit_solve(divider).text, "Series (same current): Vin – R1 – R2 (R1 + R2 = 4 kΩ).")
  end,

  thevenin_seen_by_a_load = function()
    local request = dividerWith({ id = "RL", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 3000 }, { equivalent = { load = "RL" } })
    has(
      api.circuit_solve(request).text,
      "Thevenin equivalent seen by RL, + at (2,2), − at (2,0): Vth = 3 V, Rth = 750 Ω; Norton: IN = 4 mA in parallel with Rth; a matched load (750 Ω) would absorb 3 mW."
    )
  end,

  thevenin_at_two_points_with_a_dependent_source = function()
    -- A test source finds Rth where switching sources off alone would not:
    -- i = v/1k + 2·(v/1k), so Rth = 1 kΩ / 3.
    local text = api.circuit_solve(dc({
      { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
      { id = "F1", type = "CCCS", from = { 2, 2 }, to = { 2, 0 }, gain = 2, control = { part = "R1" } },
      { id = "W1", type = "wire", from = { 0, 2 }, to = { 2, 2 } },
      { id = "W0", type = "wire", from = { 0, 0 }, to = { 2, 0 } },
      GND,
    }, { equivalent = { from = { 2, 2 }, to = { 2, 0 } } })).text
    has(text, "Vth = 0 V, Rth = 333 Ω")
  end,

  superposition_splits_a_probe_by_source = function()
    local request = table.clone(twoSources)
    request.superposition = true
    has(api.circuit_solve(request).text, "V_x = 10 V = 8 V (V_1 alone) + 2 V (I_1 alone)")
  end,

  a_source_switched_off_is_dead_and_crossed_out = function()
    local request = table.clone(twoSources)
    request.components = table.clone(twoSources.components)
    request.components[5] = { id = "I1", type = "I", from = { 4, 0 }, to = { 4, 2 }, value = 0.003, label = "I_1", off = true }
    close(api.solve(request).probes.V_x, 8, 1e-9)
    local svg = api.circuit_draw(request).artifact.svg
    has(svg, 'opacity="0.4"')
    has(svg, 'stroke="#e03131"')
  end,

  time_constant_of_one_capacitor = function()
    local text = api.circuit_solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 5 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000 },
      { id = "C1", type = "C", from = { 2, 2 }, to = { 2, 0 }, value = 1e-6 },
      { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
      GND,
    })).text
    has(text, "τ = Rth·C1 = 1 ms, with Rth = 1 kΩ seen by C1")
  end,

  nodal_equations_name_the_nodes = function()
    local request = dividerWith({ id = "RL", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 3000 }, { nodal = true })
    local text = api.circuit_solve(request).text
    has(text, "Nodal analysis: reference node = ground (0 V); unknowns: V(A), V(B).")
    has(text, "KCL at B: (V(B) − V(A))/R1 + V(B)/R2 + V(B)/RL = 0")
    has(text, "Vin fixes V(A) = 12 V")
    -- The same letters are in the list of nodes and on the figure.
    has(text, "B [R1, R2, RL] = 2.4 V")
    has(api.circuit_draw(request).artifact.svg, ">B</text>")
  end,

  nodal_equations_form_a_supernode = function()
    -- VA/2 + VB/4 = 2 and VB = VA + 4: VA = 4/3 V, VB = 16/3 V.
    local request = dc({
      { id = "I1", type = "I", from = { 0, 0 }, to = { 0, 2 }, value = 2 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 2 },
      { id = "V2", type = "V", from = { 0, 2 }, to = { 2, 2 }, value = 4 },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 4 },
      { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "VB" },
    }, { nodal = true })
    close(api.solve(request).probes.VB, 16 / 3, 1e-9)
    local text = api.circuit_solve(request).text
    has(text, "KCL over the supernode {A, B}: −I1 + V(A)/R1 + V(B)/R2 = 0")
    has(text, "V2 fixes V(B) − V(A) = 4 V")
  end,

  nodal_equations_define_a_controlling_current = function()
    local text = api.circuit_solve(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 10 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000 },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 100 },
      { id = "F1", type = "CCCS", from = { 4, 0 }, to = { 4, 2 }, gain = 2, control = { part = "R1" } },
      { id = "W1", type = "wire", from = { 2, 2 }, to = { 4, 2 } },
      { id = "W0", type = "wire", from = { 4, 0 }, to = { 0, 0 } },
      GND,
    }, { nodal = true })).text
    has(text, "− 2·i(R1) = 0")
    has(text, "where i(R1) = (V(A) − V(B))/R1")
  end,

  named_nodes_keep_their_names = function()
    local request = dividerWith({ id = "N1", type = "node", from = { 2, 2 }, label = "V_x" }, { show = { nodes = true, voltages = true } })
    local drawn = api.circuit_draw(request)
    has(drawn.text, "V_x [R1, R2] = 3 V")
    has(drawn.text, "A [R1, Vin] = 12 V")
    has(drawn.artifact.svg, "A: 12 V")
    close(api.solve(request).nodeNames.V_x, 3, 1e-9)
  end,

  -- Drawing ---------------------------------------------------------------------

  resistors_are_zigzags = function()
    local svg = api.circuit_draw(divider).artifact.svg
    lacks(svg, "<rect")
    -- R1 runs from (0,2) to (2,2): 140 px wide, its zigzag centred between them.
    has(svg, 'd="M 52 -140 L 55 -147 L 61 -133 L 67 -147 L 73 -133 L 79 -147 L 85 -133 L 88 -140"')
  end,

  labels_take_subscripts = function()
    local svg = api.circuit_draw(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 5, label = "V_in" },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000, label = "R_{load 2}" },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000, label = "a_b<c" },
      { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
      GND,
      { id = "P", type = "probe", from = { 2, 2 }, label = "V_out" },
      { id = "N", type = "terminal", from = { 0, 2 }, label = "snake_" },
    })).artifact.svg
    has(svg, '>V<tspan dy="3" font-size="9">in</tspan></text>')
    has(svg, '>R<tspan dy="3" font-size="9">load 2</tspan><tspan dx="3.6" dy="-3">1 kΩ</tspan></text>')
    -- The value after a subscripted probe name returns to the baseline.
    has(svg, '>V<tspan dy="3" font-size="9">out</tspan><tspan dy="-3">: 2.5 V</tspan></text>')
    -- Markup in a label is escaped, inside a subscript too; a trailing underscore is literal.
    has(svg, '>a<tspan dy="3" font-size="9">b</tspan><tspan dy="-3">&lt;c</tspan></text>')
    has(svg, ">snake_</text>")
  end,

  labels_sharing_a_point_do_not_overprint = function()
    -- The probe and the node letter both sit at (2,2).
    local request = table.clone(divider)
    request.show = { nodes = true, voltages = true }
    local svg = api.circuit_draw(request).artifact.svg
    local function where(text: string): (number, number)
      local x, y = svg:match('x="(%-?[%d.]+)" y="(%-?[%d.]+)">' .. text .. "</text>")
      assert(x and y, "no label `" .. text .. "` in:\n" .. svg)
      return tonumber(x) :: number, tonumber(y) :: number
    end
    local px, py = where("Vout: 3 V")
    local nx, ny = where("B: 3 V")
    assert(math.abs(py - ny) >= 12 or math.abs(px - nx) >= 60, string.format("labels at (%g,%g) and (%g,%g) overlap", px, py, nx, ny))
  end,

  a_voltage_label_moves_off_a_mesh_loop = function()
    -- Left of R2, between its + and −, is where v reads best: x = 124, right-aligned.
    local function labelX(annotations): number
      local request = table.clone(twoSources)
      request.annotations = annotations
      local x = api.circuit_draw(request).artifact.svg:match('x="(%-?[%d.]+)" y="%-?[%d.]+">vx = 10 V</text>')
      assert(x, "no voltage label")
      return tonumber(x) :: number
    end
    local mark = { type = "voltage", part = "R2", label = "vx", value = true }
    assert(labelX({ mark }) == 124, "the label is not beside the part")
    -- A loop drawn in that mesh takes the place, so the label goes elsewhere.
    assert(labelX({ { type = "loop", at = { 1, 1 }, label = "i_1" }, mark }) ~= 124, "the label is drawn over the loop")
  end,

  parallel_parts_are_drawn_side_by_side = function()
    local svg = api.circuit_draw(dividerWith({ id = "RL", type = "R", from = { 2, 0 }, to = { 2, 2 }, value = 3000 })).artifact.svg
    -- R2 and RL join the same points (RL the other way round): one is drawn
    -- 35 px left of them, the other 35 px right.
    has(svg, "M 105 -88")
    has(svg, "M 175 -52")
  end,

  dependent_sources_are_diamonds_with_their_law = function()
    local svg = api.circuit_draw(dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 10 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 2, label = "R_1" },
      { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 4 },
      { id = "G1", type = "VCCS", from = { 4, 0 }, to = { 4, 2 }, gain = 0.5, control = { part = "R1" } },
      { id = "W1", type = "wire", from = { 2, 2 }, to = { 4, 2 } },
      { id = "W0", type = "wire", from = { 4, 0 }, to = { 0, 0 } },
      GND,
    })).artifact.svg
    -- The diamond around (4,1), and the controlling voltage named on R_1.
    has(svg, 'd="M 280 -53 L 263 -70 L 280 -87 L 297 -70 Z"')
    has(svg, '>500 mS·v<tspan dy="3" font-size="9">R1</tspan></text>')
    has(svg, 'fill="#e03131"')
  end,

  current_arrows_point_the_way_the_current_flows = function()
    -- R2 is given bottom-up, so its from → to current is −3 mA: the arrow is
    -- reversed and the label positive.
    local components = table.clone(divider.components)
    components[3] = { id = "R2", type = "R", from = { 2, 0 }, to = { 2, 2 }, value = 1000, label = "R2" }
    local request = dc(components, { show = { currents = true } })
    close(api.solve(request).currents.R2, -0.003, 1e-12)
    local svg = api.circuit_draw(request).artifact.svg
    has(svg, ">3 mA</text>")
    lacks(svg, "−3 mA")
  end,

  annotations_are_drawn_and_never_solved = function()
    local request = table.clone(twoSources)
    request.annotations = {
      { type = "path", points = { { 0, 0 }, { 0, 2 }, { 2, 2 }, { 2, 0 } }, closed = true, label = "i_a" },
      { type = "loop", at = { 3, 1 }, label = "i_2", direction = "ccw" },
      { type = "voltage", part = "R2", label = "v_2", value = true },
      { type = "current", part = "R3", label = "i_3", value = true, reverse = true },
      { type = "cross", part = "R3" },
      { type = "highlight", parts = { "R3", "I1" }, label = "branch", color = "purple" },
      { type = "voltage", from = { 4, 2 }, to = { 4, 0 }, label = "v_{oc}" },
      { type = "note", at = { 1, 2.6 }, text = "KVL here" },
      { type = "bogus" },
      { type = "current", part = "nope" },
    }
    local drawn = api.circuit_draw(request)
    local svg = drawn.artifact.svg
    has(svg, 'stroke-opacity="0.3"') -- the path band
    has(svg, "A 17 17 0 1 0") -- the counter-clockwise loop
    has(svg, '<tspan dx="3.6" dy="-3">= 10 V</tspan>') -- v_2 with its value
    has(svg, '<tspan dx="3.6" dy="-3">= 3 mA</tspan>') -- i_3: all of I_1, counted against R3's from → to
    has(svg, 'stroke-dasharray="6 4"')
    has(svg, 'fill="#7048e8"')
    has(svg, ">KVL here</text>")
    close(api.solve(request).probes.V_x, 10, 1e-9)
    -- What was drawn is stored, so the view redraws the same figure.
    assert(api.render(drawn.artifact.data) == svg, "re-render differs from the stored figure")
  end,

  empty_and_malformed_requests_do_not_crash = function()
    assert(api.circuit_draw({}).artifact.svg:find("<svg", 1, true))
    local out = api.circuit_draw(dc({
      { id = "R1", type = "R", from = { 0, 0 }, to = { 0, 0 }, value = 10 },
      { id = "R2", type = "R", from = { "a", 1 }, to = { 1, 1 }, value = 10 },
      { id = "P", type = "probe" },
      { type = "wire", from = { 0, 0 }, to = { 0.4, 0.2 } },
    }, { annotations = { { type = "path", points = { { 0, 0 } } }, { type = "loop" }, { type = "cross" } } }))
    has(out.text, "Warnings:")
  end,

  golden_divider_svg = function()
    local drawn = api.circuit_draw(divider)
    assert(drawn.artifact.kind == "svg", "circuit_draw returns an svg artifact")
    test.golden("divider.svg", drawn.artifact.svg)
  end,

  golden_nodal_text = function()
    local request = dividerWith({ id = "RL", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 3000, label = "R_L" }, {
      nodal = true,
      equivalent = { load = "RL" },
    })
    test.golden("nodal.txt", api.circuit_solve(request).text .. "\n")
  end,

  golden_annotated_svg = function()
    local request = table.clone(twoSources)
    request.show = { nodes = true, voltages = true, currents = true }
    request.annotations = {
      { type = "loop", at = { 1, 1 }, label = "i_1" },
      { type = "loop", at = { 3, 1 }, label = "i_2" },
      { type = "highlight", parts = { "R2" }, label = "shared branch" },
    }
    test.golden("annotated.svg", api.circuit_draw(request).artifact.svg)
  end,

  -- Grading ---------------------------------------------------------------------

  grade_resolves_submitted_values = function()
    local target = { probe = "Vout", equals = 3.0, tolerance = 0.05 }
    local ok = api.grade({ components = divider.components, analysis = divider.analysis }, target)
    assert(ok.correct == true, ok.detail)
    -- Changing R2 changes the re-solved probe, whatever probes the view claims.
    local changed = table.clone(divider.components)
    changed[3] = { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 3000, label = "R2" }
    local bad = api.grade({ components = changed, analysis = divider.analysis, probes = { Vout = 3.0 } }, target)
    assert(bad.correct == false, bad.detail)
    local comp = api.grade({ components = changed }, { component = "R2", equals = 3000 })
    assert(comp.correct == true, comp.detail)
  end,

  grade_reads_a_current_probe_and_a_gain = function()
    local components = table.clone(divider.components)
    table.insert(components, { id = "Pi", type = "probe", part = "R2", quantity = "current", label = "i_2" })
    local ok = api.grade({ components = components, analysis = divider.analysis }, { probe = "i_2", equals = 0.003 })
    assert(ok.correct == true, ok.detail)
    has(ok.detail, "i_2 is 3 mA; expected 3 mA")
    local gain = api.grade(
      { components = { { id = "G1", type = "VCCS", from = { 0, 0 }, to = { 0, 2 }, gain = 0.5, control = { part = "R1" } } } },
      { component = "G1", equals = 0.5 }
    )
    assert(gain.correct == true, gain.detail)
  end,

  grade_complex_target = function()
    local circuit = lowpass()
    local answer = { components = circuit.components, analysis = circuit.analysis }
    local ok = api.grade(answer, { probe = "Vout", equals = 0.7071, tolerance = 0.01 })
    assert(ok.correct == true, ok.detail)
    local bad = api.grade(answer, { probe = "Vout", equals = 1.0, tolerance = 0.01 })
    assert(bad.correct == false, bad.detail)
  end,

  grade_zero_target_uses_floor = function()
    local balanced = dc({
      { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
      { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
      GND,
      { id = "P", type = "probe", from = { 0, 2 }, to = { 0, 2 }, label = "Vzero" },
    })
    local ok = api.grade(balanced, { probe = "Vzero", equals = 0, floor = 0.01 })
    assert(ok.correct == true, ok.detail)
  end,
}
