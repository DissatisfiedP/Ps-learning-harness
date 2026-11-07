-- Circuit fixtures. Run with `cargo run -p harness --bin plugin -- test plugins/circuit`.
local api = require("main")

local function close(got, want, tol)
  tol = tol or 1e-6
  assert(type(got) == "number", "expected a real number, got " .. tostring(got))
  assert(math.abs(got - want) <= tol, string.format("expected %.9g, got %.9g", want, got))
end

local function warnings(result)
  return table.concat(result.warnings or {}, "; ")
end

local divider = {
  analysis = { dc = true },
  components = {
    { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 12, label = "Vin" },
    { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 3000, label = "R1" },
    { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000, label = "R2" },
    { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
    { id = "GND", type = "ground", from = { 0, 0 }, to = { 0, 0 } },
    { id = "P1", type = "probe", from = { 2, 2 }, to = { 2, 0 }, label = "Vout" },
  },
}

local amp = {
  analysis = { dc = true },
  components = {
    { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
    { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000 },
    { id = "R2", type = "R", from = { 2, 2 }, to = { 4, 2 }, value = 10000 },
    { id = "U1", type = "opamp", from = { 2, 4 }, to = { 2, 2 }, third = { 4, 2 } },
    { id = "W1", type = "wire", from = { 2, 4 }, to = { 0, 4 } },
    { id = "W2", type = "wire", from = { 0, 4 }, to = { 0, 0 } },
    { id = "GND", type = "ground", from = { 0, 0 } },
    { id = "P1", type = "probe", from = { 4, 2 }, label = "Vout" },
  },
}

local R_LP, C_LP = 1000, 1e-6
local lowpass = {
  analysis = { ac = { frequencyHz = 1 / (2 * math.pi * R_LP * C_LP) } },
  components = {
    { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, ac = { magnitude = 1, phaseDeg = 0 } },
    { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = R_LP },
    { id = "C1", type = "C", from = { 2, 2 }, to = { 2, 0 }, value = C_LP },
    { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
    { id = "GND", type = "ground", from = { 0, 0 } },
    { id = "P1", type = "probe", from = { 2, 2 }, to = { 2, 0 }, label = "Vout" },
  },
}

local function tests()
  return {
    divider = function()
      local out = api.solve(divider)
      close(out.probes.Vout, 3, 1e-6)
      assert(warnings(out) == "", warnings(out))
    end,

    loaded_divider = function()
      local loaded = {
        analysis = { dc = true },
        components = {
          { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 12 },
          { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 3000 },
          { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000 },
          { id = "RL", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 3000 },
          { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
          { id = "GND", type = "ground", from = { 0, 0 }, to = { 0, 0 } },
          { id = "P1", type = "probe", from = { 2, 2 }, to = { 2, 0 }, label = "Vout" },
        },
      }
      close(api.solve(loaded).probes.Vout, 2.4, 1e-6)
    end,

    rc_lowpass_cutoff = function()
      local out = api.solve(lowpass)
      close(host.num.abs(out.probes.Vout), 1 / math.sqrt(2), 1e-4)
      close(host.num.phaseDeg(out.probes.Vout), -45, 1e-3)
    end,

    rc_cutoff_text_is_a_phasor = function()
      local text = api.circuit_solve(lowpass).text
      assert(text:find("Vout = 707 mV ∠ −45°", 1, true), text)
      local drawn = api.circuit_draw(lowpass)
      assert(drawn.text:find("Vout = 707 mV ∠ −45°", 1, true), drawn.text)
      assert(drawn.artifact.svg:find("Vout: 707 mV ∠ −45°", 1, true), "SVG probe label is not a phasor")
    end,

    dc_text_reports_nodes_and_currents = function()
      local text = api.circuit_solve(divider).text
      assert(text:find("Vout = 3 V", 1, true), text)
      assert(text:find("node R1–R2 = 3 V", 1, true), text)
      assert(text:find("node R1–Vin = 12 V", 1, true), text)
      -- 12 V across 4 kΩ: 3 mA through R1 and R2; inside the source it flows − to +, i.e. from → to.
      assert(text:find("R1 = 3 mA", 1, true), text)
      assert(text:find("R2 = 3 mA", 1, true), text)
      assert(text:find("Vin = 3 mA", 1, true), text)
    end,

    current_source_label_in_amps = function()
      local src = {
        analysis = { dc = true },
        components = {
          { id = "I1", type = "I", from = { 0, 0 }, to = { 0, 2 }, value = 0.002, label = "I1" },
          { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
          { id = "GND", type = "ground", from = { 0, 0 } },
          { id = "P", type = "probe", from = { 0, 2 }, label = "Vr" },
        },
      }
      local drawn = api.circuit_draw(src)
      assert(drawn.artifact.svg:find("I1 2 mA", 1, true), "I source not labelled in A")
      close(api.solve(src).probes.Vr, 2, 1e-9)
      local small = api.circuit_draw({ analysis = { dc = true }, components = {
        { id = "C1", type = "C", from = { 0, 0 }, to = { 0, 2 }, value = 4.7e-6 },
        { id = "GND", type = "ground", from = { 0, 0 } },
      } })
      assert(small.artifact.svg:find("4.7 µF", 1, true), "micro prefix is not µ")
    end,

    ground_ignores_to_and_probe_defaults_to_ground = function()
      local odd = {
        analysis = { dc = true },
        components = {
          { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 12 },
          { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 3000 },
          { id = "R2", type = "R", from = { 2, 2 }, to = { 2, 0 }, value = 1000 },
          { id = "W0", type = "wire", from = { 2, 0 }, to = { 0, 0 } },
          { id = "GND", type = "ground", from = { 0, 0 }, to = { 0, 1 } },
          { id = "P1", type = "probe", from = { 2, 2 }, label = "Vout" },
          { id = "P2", type = "probe", from = { 0, 2 }, to = { 2, 2 }, label = "VR1" },
        },
      }
      local out = api.solve(odd)
      close(out.probes.Vout, 3, 1e-6)
      close(out.probes.VR1, 9, 1e-6)
      assert(warnings(out) == "", warnings(out))
    end,

    inverting_opamp = function()
      close(api.solve(amp).probes.Vout, -10, 1e-3)
    end,

    inverting_opamp_drawn_connected = function()
      local svg = api.circuit_draw(amp).artifact.svg
      -- Grid → px: x = 50 + 70·gx, y = 50 + 70·gy (minimum point is 0,0).
      -- − input lead starts at `to` (2,2) = (190,190); output lead ends at `third` (4,2) = (330,190);
      -- + input lead starts at `from` (2,4) = (190,330).
      assert(svg:find('d="M 190 190 V', 1, true), "no lead from the − input point")
      assert(svg:find('d="M 190 330 V', 1, true), "no lead from the + input point")
      assert(svg:find("H 330 V 190", 1, true), "no lead to the output point")
    end,

    opamp_output_on_lone_point_does_not_crash = function()
      local out = api.solve({ analysis = { dc = true }, components = {
        { id = "U1", type = "opamp", from = { 0, 0 }, to = { 0, 0 }, third = { 5, 5 } },
        { id = "GND", type = "ground", from = { 0, 0 } },
      } })
      assert(warnings(out):find("dangling") ~= nil, warnings(out))
    end,

    floating_node_warns = function()
      local floating = {
        analysis = { dc = true },
        components = {
          { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
          { id = "GND", type = "ground", from = { 0, 0 }, to = { 0, 0 } },
          { id = "R1", type = "R", from = { 2, 2 }, to = { 4, 2 }, value = 1000 },
        },
      }
      local out = api.solve(floating)
      assert(warnings(out):find("dangling") ~= nil, warnings(out))
    end,

    probe_does_not_hide_dangling_node = function()
      local out = api.solve({ analysis = { dc = true }, components = {
        { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
        { id = "R1", type = "R", from = { 0, 2 }, to = { 2, 2 }, value = 1000 },
        { id = "GND", type = "ground", from = { 0, 0 } },
        { id = "P", type = "probe", from = { 2, 2 }, label = "Vend" },
      } })
      assert(warnings(out):find("dangling") ~= nil, warnings(out))
    end,

    non_positive_values_warn = function()
      for _, v in ipairs({ 0, -100 }) do
        local out = api.solve({ analysis = { dc = true }, components = {
          { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
          { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
          { id = "R0", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = v },
          { id = "GND", type = "ground", from = { 0, 0 } },
        } })
        assert(warnings(out):find("`R0` needs a positive value", 1, true), warnings(out))
      end
    end,

    hidden_probe_omits_value = function()
      local hidden = table.clone(divider)
      hidden.components = table.clone(divider.components)
      hidden.components[6] = { id = "P1", type = "probe", from = { 2, 2 }, to = { 2, 0 }, label = "Vout", hidden = true }
      local drawn = api.circuit_draw(hidden)
      assert(drawn.artifact.svg:find("Vout: ?", 1, true), "hidden probe is not drawn as ?")
      assert(not drawn.artifact.svg:find("Vout: 3", 1, true), "hidden probe value in SVG")
      assert(drawn.artifact.data.result.probes.Vout == nil, "hidden probe value in artifact data")
      assert(api.solve(hidden).probes.Vout == nil, "hidden probe value in view solve")
      assert(api.render(drawn.artifact.data):find("Vout: ?", 1, true), "re-render shows the value")
      -- The model still gets it.
      assert(drawn.text:find("Vout (hidden from the learner) = 3 V", 1, true), drawn.text)
    end,

    golden_divider_svg = function()
      local drawn = api.circuit_draw(divider)
      assert(drawn.artifact.kind == "svg", "circuit_draw returns an svg artifact")
      host.test.golden("divider.svg", drawn.artifact.svg)
    end,

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

    grade_complex_target = function()
      local answer = { components = lowpass.components, analysis = lowpass.analysis }
      local ok = api.grade(answer, { probe = "Vout", equals = 0.7071, tolerance = 0.01 })
      assert(ok.correct == true, ok.detail)
      local bad = api.grade(answer, { probe = "Vout", equals = 1.0, tolerance = 0.01 })
      assert(bad.correct == false, bad.detail)
    end,

    grade_zero_target_uses_floor = function()
      local balanced = { analysis = { dc = true }, components = {
        { id = "V1", type = "V", from = { 0, 0 }, to = { 0, 2 }, value = 1 },
        { id = "R1", type = "R", from = { 0, 2 }, to = { 0, 0 }, value = 1000 },
        { id = "GND", type = "ground", from = { 0, 0 } },
        { id = "P", type = "probe", from = { 0, 2 }, to = { 0, 2 }, label = "Vzero" },
      } }
      local ok = api.grade(balanced, { probe = "Vzero", equals = 0, floor = 0.01 })
      assert(ok.correct == true, ok.detail)
    end,
  }
end

return tests()
