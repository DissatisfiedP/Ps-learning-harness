-- Argument map fixtures. Run with `cargo run -p harness --bin plugin -- test plugins/argument-map`.
local api = require("../src/main")
local wrap = require("../src/wrap")
local test = require("@harness/test")

local function hume(blanks: { string }?)
  return {
    title = "Hume on induction",
    conclusion = "Inductive inference has no rational justification.",
    premises = {
      { id = "P1", text = "Every inference from observed cases to unobserved ones assumes that nature is uniform." },
      { id = "P2", text = "That assumption can't be proved by pure reasoning, since denying it involves no contradiction." },
      { id = "P3", text = "It can't be supported by experience either, because that would use induction to justify induction." },
    },
    objections = {
      {
        id = "O1",
        targets = "P3",
        text = "Induction has worked every time so far, so it is reliable.",
        replies = { { by = "Hume", text = "That is exactly the circular move P3 describes." } },
      },
      {
        id = "O2",
        targets = "conclusion",
        by = "Strawson",
        text = "Asking for a justification of induction as a whole is a confusion: being inductively supported is what being reasonable means here.",
      },
    },
    blanks = blanks,
  }
end

local function errorOf(f: () -> ()): string
  local ok, err = pcall(f)
  assert(not ok, "expected an error")
  return tostring(err)
end

return {
  golden_hume_card = function()
    local out = api.show(hume())
    assert(out.artifact.kind == "svg", "argument_map_show returns an svg artifact")
    test.golden("hume.svg", out.artifact.svg)
  end,

  golden_hume_blanks_card = function()
    test.golden("hume-blanks.svg", api.show(hume({ "P3", "O1.1", "conclusion" })).artifact.svg)
  end,

  summary_names_counts_and_attacks = function()
    local text = api.show(hume()).text
    assert(text:find("Argument map “Hume on induction”: 3 premises, a conclusion, 2 objections, 1 reply.", 1, true), text)
    assert(text:find("Attacks: O1 → P3, O2 → conclusion.", 1, true), text)
    assert(text:find("Nothing is hidden.", 1, true), text)
  end,

  objections_sit_under_their_target = function()
    local svg = api.show(hume()).artifact.svg
    local p3 = assert(svg:find(">P3<", 1, true), "no P3 label")
    local o1 = assert(svg:find("Objection to P3", 1, true), "O1 heading missing")
    local reply = assert(svg:find("Reply — Hume", 1, true), "reply heading missing")
    local conclusion = assert(svg:find(">∴<", 1, true), "no conclusion mark")
    local o2 = assert(svg:find("Objection to the inference — Strawson", 1, true), "O2 heading missing")
    assert(p3 < o1 and o1 < reply and reply < conclusion and conclusion < o2, "card order is wrong")
  end,

  blanks_hide_text_from_the_learner_but_not_the_model = function()
    local out = api.show(hume({ "P3", "O1.1", "conclusion" }))
    local svg = out.artifact.svg
    assert(not svg:find("use induction to justify", 1, true), "hidden premise text in the SVG")
    assert(not svg:find("circular move", 1, true), "hidden reply text in the SVG")
    assert(not svg:find("no rational justification", 1, true), "hidden conclusion in the SVG")
    local _, marks = svg:gsub("%? %(fill in%)", "")
    assert(marks == 3, "expected 3 blanks drawn, got " .. marks)
    -- The headings stay: the learner sees what kind of part is missing.
    assert(svg:find("Reply — Hume", 1, true), "hidden reply lost its heading")

    local data = out.artifact.data
    assert(data.premises[3].hidden == true and data.premises[3].text == nil, "hidden premise text in the data")
    assert(data.objections[1].replies[1].text == nil, "hidden reply text in the data")
    assert(data.conclusion.text == nil, "hidden conclusion in the data")
    assert(data.premises[1].text ~= nil and data.premises[1].hidden == nil, "visible premise lost its text")

    assert(out.text:find("Hidden from the learner (shown as ?): P3 = “It can't be supported", 1, true), out.text)
    assert(out.text:find("O1.1 = “That is exactly the circular move P3 describes.”", 1, true), out.text)
    assert(out.text:find("conclusion = “Inductive inference has no rational justification.”", 1, true), out.text)
  end,

  render_redraws_the_stored_card = function()
    for _, blanks in { {}, { "P2", "O2" } } do
      local out = api.show(hume(blanks))
      assert(api.render(out.artifact.data) == out.artifact.svg, "render(data) differs from the tool's figure")
    end
  end,

  unknown_target_is_refused = function()
    local bad = hume()
    bad.objections[1].targets = "P9"
    local err = errorOf(function()
      api.show(bad)
    end)
    assert(err:find("objection `O1` targets `P9`, which is not a premise id", 1, true), err)
  end,

  unknown_blank_is_refused = function()
    local err = errorOf(function()
      api.show(hume({ "O1.2" }))
    end)
    assert(err:find("blank `O1.2` is not a premise, objection or reply id", 1, true), err)
  end,

  duplicate_ids_are_refused = function()
    local bad = hume()
    bad.objections[2].id = "P1"
    local err = errorOf(function()
      api.show(bad)
    end)
    assert(err:find("id `P1` is used twice (premise 1 and objection 2)", 1, true), err)
    local reserved = hume()
    reserved.premises[1].id = "conclusion"
    err = errorOf(function()
      api.show(reserved)
    end)
    assert(err:find("id `conclusion` is used twice", 1, true), err)
  end,

  empty_argument_is_refused = function()
    local err = errorOf(function()
      api.show({ title = "Nothing", conclusion = "So.", premises = {} })
    end)
    assert(err:find("at least one premise", 1, true), err)
    err = errorOf(function()
      api.show({ title = "Blank", conclusion = "So.", premises = { { id = "P1", text = "   " } } })
    end)
    assert(err:find("premise `P1` needs non-empty text", 1, true), err)
  end,

  objections_are_optional = function()
    local out = api.show({ title = "Modus ponens", conclusion = "Q.", premises = { { id = "P1", text = "If P then Q." }, { id = "P2", text = "P." } } })
    assert(out.text:find("2 premises, a conclusion, 0 objections, 0 replies.", 1, true), out.text)
    assert(not out.text:find("Attacks:", 1, true), out.text)
  end,

  long_text_wraps_inside_the_box = function()
    local long = string.rep("contradiction ", 30)
    local svg = api.show({ title = "Long", conclusion = "So.", premises = { { id = "P1", text = long } } }).artifact.svg
    local _, lines = svg:gsub(">contradiction contradiction", "")
    assert(lines >= 6, "30 words should wrap onto several lines, got " .. lines)
  end,

  wrap_counts_characters_not_bytes = function()
    -- 10 two-byte characters fit a width of 10.
    local lines = wrap.lines("éééééééééé ü", 10)
    assert(#lines == 2 and lines[1] == "éééééééééé" and lines[2] == "ü", table.concat(lines, "|"))
    -- An over-long word is cut on character boundaries.
    local cut = wrap.lines("ααααααα", 3)
    assert(#cut == 3 and cut[1] == "ααα" and cut[3] == "α", table.concat(cut, "|"))
    assert(utf8.len(cut[2]) == 3, "a piece was cut inside a character")
    assert(#wrap.lines("  \n ", 10) == 0, "whitespace-only text gives no lines")
  end,
}
