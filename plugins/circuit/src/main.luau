-- Reference plugin: DC and single-frequency AC circuits.
-- Modified nodal analysis on host.num; SVG schematic; view solver; grader.
--
-- Part geometry: every part except `ground` and `probe` joins `from` to `to`.
-- `V` raises `to` above `from`; `I` pushes its current from `from` to `to`;
-- an op-amp has `from` = + input, `to` = − input, `third` = output.
-- `ground` sits at `from` only. A `probe` reads V(from) − V(to), with `to`
-- defaulting to ground. Branch currents are reported flowing from `from` to
-- `to` through the part (op-amp: out of the output pin).
local svg = host.svg
local num = host.num

local GRID = 70
local MARGIN = 50

local function pt(p)
  if type(p) ~= "table" or #p < 2 then return nil end
  return math.floor(p[1] + 0.5), math.floor(p[2] + 0.5)
end

local function key(p)
  local x, y = pt(p)
  if not x then return nil end
  return x .. "," .. y
end

local function name_of(c)
  return c.label or c.id or c.type
end

local function signed(text)
  return (text:gsub("^%-", "−"))
end

-- Engineering notation of a real number: "4.7 kΩ", "−10 V", "707 mV".
local function eng(v, unit)
  unit = unit or ""
  if math.abs(v) < 1e-12 then return "0 " .. unit end
  local sign = v < 0 and "−" or ""
  v = math.abs(v)
  local exps = { { 1e9, "G" }, { 1e6, "M" }, { 1e3, "k" }, { 1, "" }, { 1e-3, "m" }, { 1e-6, "µ" }, { 1e-9, "n" }, { 1e-12, "p" } }
  for _, e in ipairs(exps) do
    if v >= e[1] then
      return sign .. string.format("%.3g", v / e[1]) .. " " .. e[2] .. unit
    end
  end
  return sign .. string.format("%.3g", v) .. " " .. unit
end

-- A solved value: plain for real numbers, magnitude ∠ phase for phasors.
local function fmt(value, unit)
  if type(value) == "table" then
    return eng(num.abs(value), unit) .. " ∠ " .. signed(string.format("%.3g", num.phaseDeg(value))) .. "°"
  end
  return eng(value, unit)
end

local UNITS = { R = "Ω", C = "F", L = "H", V = "V", I = "A" }

-- ---- netlist --------------------------------------------------------------

local function union(parent, a, b)
  local ra, rb = a, b
  while parent[ra] ~= ra do ra = parent[ra] end
  while parent[rb] ~= rb do rb = parent[rb] end
  if ra ~= rb then parent[rb] = ra end
end

local function find(parent, a)
  local r = a
  while parent[r] ~= r do r = parent[r] end
  while parent[a] ~= r do
    local next = parent[a]
    parent[a] = r
    a = next
  end
  return r
end

local function analysis_of(input)
  local a = input.analysis or { dc = true }
  if a.ac then return "ac", 2 * math.pi * (tonumber(a.ac.frequencyHz) or 0) end
  return "dc", 0
end

local function source_value(c, kind)
  if kind == "dc" then return tonumber(c.value) or 0 end
  local ac = c.ac or {}
  local mag = tonumber(ac.magnitude)
  if not mag then mag = tonumber(c.value) or 0 end
  local phase = math.rad(tonumber(ac.phaseDeg) or 0)
  return { re = mag * math.cos(phase), im = mag * math.sin(phase) }
end

local function add(mat, i, j, v)
  if i <= 0 or j <= 0 then return end
  mat[i][j] = num.add(mat[i][j], v)
end

local function passive(t)
  return t == "R" or t == "C" or t == "L"
end

-- Solve `input`; returns { probes, hidden, nodes, nodeNames, currents, warnings }.
local function solve_netlist(input)
  local kind, w = analysis_of(input)
  local comps = input.components or {}
  local warnings = {}
  local function empty()
    return { probes = {}, hidden = {}, nodes = {}, nodeNames = {}, currents = {}, warnings = warnings }
  end
  local parent = {}
  local keys = {}
  local ground = nil
  local function ensure(k)
    if k and not parent[k] then
      parent[k] = k
      keys[#keys + 1] = k
    end
    return k
  end
  local bad = {}
  for _, c in ipairs(comps) do
    local t = c.type
    if passive(t) then
      local v = tonumber(c.value)
      if not v or v <= 0 then
        bad[c] = true
        warnings[#warnings + 1] = "`" .. name_of(c) .. "` needs a positive value; it is left out of the solution"
      end
    end
    if t == "ground" then
      local a = ensure(key(c.from))
      if a then
        ground = ground or a
        union(parent, a, ground)
      end
    elseif t ~= "probe" then
      local a, b = ensure(key(c.from)), ensure(key(c.to))
      if t == "opamp" then ensure(key(c.third)) end
      if t == "wire" or (t == "switch" and c.closed ~= false) then
        if a and b then union(parent, a, b) end
      end
    end
  end
  if not ground then
    warnings[#warnings + 1] = "no ground: add a `ground` component"
    ground = keys[1] or "0,0"
    ensure(ground)
  end
  local index = {}
  local count = 0
  -- 0 = ground, nil = a point no part touches.
  local function node(k)
    if not k or not parent[k] then return nil end
    local r = find(parent, k)
    if r == find(parent, ground) then return 0 end
    if not index[r] then
      count = count + 1
      index[r] = count
    end
    return index[r]
  end
  for _, k in ipairs(keys) do node(k) end

  -- Names for nodes: the labels of the parts that touch them ("node R1–R2").
  local touching = {}
  for _, c in ipairs(comps) do
    local t = c.type
    if t ~= "wire" and t ~= "ground" and t ~= "probe" and t ~= "switch" then
      for _, p in ipairs({ c.from, c.to, t == "opamp" and c.third or nil }) do
        local i = node(key(p))
        if i and i > 0 then
          touching[i] = touching[i] or {}
          touching[i][name_of(c)] = true
        end
      end
    end
  end
  local names = {}
  for k, i in pairs(index) do
    local list = {}
    for label in pairs(touching[i] or {}) do list[#list + 1] = label end
    table.sort(list)
    names[i] = #list > 0 and ("node " .. table.concat(list, "–")) or ("node (" .. k .. ")")
  end
  -- Two nodes touched by the same parts (e.g. both ends of a lone R1) also get their point.
  local seen = {}
  for _, name in pairs(names) do seen[name] = (seen[name] or 0) + 1 end
  for k, i in pairs(index) do
    if seen[names[i]] > 1 then names[i] = names[i] .. " (" .. k .. ")" end
  end

  -- Voltage sources, op-amps and DC inductors (0 V sources) get a current unknown.
  local sources = {}
  for _, c in ipairs(comps) do
    if not bad[c] and (c.type == "V" or c.type == "opamp" or (c.type == "L" and kind == "dc")) then
      sources[#sources + 1] = c
    end
  end
  local n = count + #sources
  if n == 0 then return empty() end
  local zero_c = kind == "dc" and 0 or { re = 0, im = 0 }
  local mat, rhs = {}, {}
  for i = 1, n do
    mat[i] = {}
    for j = 1, n do mat[i][j] = zero_c end
    rhs[i] = zero_c
  end
  -- Side table, so the caller's components (stored in the artifact) stay unmodified.
  local branch = {}
  for i, c in ipairs(sources) do branch[c] = count + i end

  local used = {}
  local degree = {}
  local function touch(i)
    if i and i > 0 then
      used[i] = true
      degree[i] = (degree[i] or 0) + 1
    end
  end
  local function admit(a, b, y)
    add(mat, a, a, y); add(mat, b, b, y); add(mat, a, b, num.mul(y, -1)); add(mat, b, a, num.mul(y, -1))
  end
  local admittance = {}
  for _, c in ipairs(comps) do
    local t = c.type
    local a, b = node(key(c.from)), node(key(c.to))
    if t ~= "probe" and t ~= "ground" and not bad[c] then
      -- Wires and closed switches merge their ends; they connect nothing new.
      if t ~= "wire" and not (t == "switch" and c.closed ~= false) then
        touch(a); touch(b)
      end
      if t == "opamp" then touch(node(key(c.third))) end
    end
    a, b = a or 0, b or 0
    if bad[c] then
      -- left out; already warned
    elseif t == "V" and a == b then
      warnings[#warnings + 1] = "voltage source `" .. name_of(c) .. "` is shorted"
    end
    if bad[c] then
      -- nothing to stamp
    elseif t == "R" then
      admittance[c] = 1 / tonumber(c.value)
      admit(a, b, admittance[c])
    elseif t == "C" then
      admittance[c] = kind == "ac" and { re = 0, im = w * tonumber(c.value) } or 0
      if kind == "ac" then admit(a, b, admittance[c]) end
    elseif t == "L" and kind == "ac" then
      admittance[c] = num.div(1, { re = 0, im = w * tonumber(c.value) })
      admit(a, b, admittance[c])
    elseif t == "V" or t == "L" then
      local k = branch[c]
      -- Raises `to` above `from` by the value (a DC inductor is a 0 V source).
      add(mat, a, k, 1); add(mat, b, k, -1)
      add(mat, k, a, -1); add(mat, k, b, 1)
      if t == "V" then rhs[k] = num.add(rhs[k], source_value(c, kind)) end
    elseif t == "I" then
      local i0 = source_value(c, kind)
      if a > 0 then rhs[a] = num.sub(rhs[a], i0) end
      if b > 0 then rhs[b] = num.add(rhs[b], i0) end
    elseif t == "opamp" then
      local k = branch[c]
      local third = node(key(c.third)) or 0
      if third == 0 then
        warnings[#warnings + 1] = "op-amp `" .. name_of(c) .. "` needs its output at `third`, off ground"
        mat[k][k] = kind == "dc" and 1 or { re = 1, im = 0 }
      else
        -- Row k: Vout − A(V+ − V−) = 0 (an ideal op-amp is the large-gain limit).
        local A = 1e6
        add(mat, k, third, 1)
        add(mat, k, a, -A)
        add(mat, k, b, A)
        add(mat, third, k, 1)
      end
    end
  end

  for i = 1, count do
    if not used[i] then
      warnings[#warnings + 1] = "`" .. names[i] .. "` is not connected to anything"
      mat[i][i] = kind == "dc" and 1 or { re = 1, im = 0 }
    elseif (degree[i] or 0) < 2 then
      warnings[#warnings + 1] = "`" .. names[i] .. "` is dangling: only one part connects to it"
    end
  end

  local ok, x = pcall(num.linsolve, mat, rhs)
  if not ok or not x then
    warnings[#warnings + 1] = "the circuit has no unique solution (check sources and grounds)"
    return empty()
  end
  local function volt(i)
    return (i and i > 0) and x[i] or 0
  end

  local nodes = {}
  for _, k in ipairs(keys) do nodes[k] = volt(node(k)) end
  local nodeNames = {}
  for i = 1, count do nodeNames[names[i]] = x[i] end

  local currents = {}
  for _, c in ipairs(comps) do
    local t = c.type
    if not bad[c] and (passive(t) or t == "V" or t == "I" or t == "opamp") then
      local drop = num.sub(volt(node(key(c.from))), volt(node(key(c.to))))
      local i
      if branch[c] then
        i = x[branch[c]]
        if t == "opamp" then i = num.mul(i, -1) end
      elseif t == "I" then
        i = source_value(c, kind)
      else
        i = num.mul(drop, admittance[c])
      end
      currents[name_of(c)] = i
    end
  end

  local probes, hidden = {}, {}
  for _, c in ipairs(comps) do
    if c.type == "probe" then
      local label = c.label or c.id or "probe"
      local a = node(key(c.from))
      local b = c.to and node(key(c.to)) or 0
      if a == nil or b == nil then
        warnings[#warnings + 1] = "probe `" .. label .. "` is not on a node any part touches"
      else
        probes[label] = num.sub(volt(a), volt(b))
        if c.hidden then hidden[label] = true end
      end
    end
  end
  return { probes = probes, hidden = hidden, nodes = nodes, nodeNames = nodeNames, currents = currents, warnings = warnings }
end

-- ---- drawing --------------------------------------------------------------

local function pos(p, minx, miny)
  local x, y = pt(p)
  return MARGIN + (x - minx) * GRID, MARGIN + (y - miny) * GRID
end

local function label_for(c)
  local v = tonumber(c.value)
  local unit = UNITS[c.type]
  if v and unit then return name_of(c) .. " " .. eng(v, unit) end
  return name_of(c)
end

local function symbols(c, x1, y1, x2, y2, x3, y3, result)
  local mx, my = (x1 + x2) / 2, (y1 + y2) / 2
  local horizontal = math.abs(y2 - y1) < 1
  local parts = {}
  local function line(ax, ay, bx, by, w)
    parts[#parts + 1] = svg.line(ax, ay, bx, by, { ["stroke-width"] = w or 1.5, ["stroke-linecap"] = "round" })
  end
  local function lead(d)
    parts[#parts + 1] = svg.path(d, { ["stroke-width"] = 1.5, ["stroke-linecap"] = "round", ["stroke-linejoin"] = "round" })
  end
  local function text(tx, ty, value, anchor)
    parts[#parts + 1] = svg.text(tx, ty, value, { ["font-size"] = 12, ["font-family"] = "system-ui, sans-serif", fill = "currentColor", ["text-anchor"] = anchor or "middle", stroke = "none" })
  end
  if c.type == "wire" or c.type == "switch" then
    line(x1, y1, x2, y2)
    if c.type == "switch" then
      local dx, dy = (x2 - x1) * 0.25, (y2 - y1) * 0.25
      line(mx - dx, my - dy, mx + dx, my - dy)
      line(mx + dx, my - dy, mx + dx * 2, my + dy * 2)
    end
    return parts
  end
  if c.type == "ground" then
    line(x1, y1, x1, y1 + 12)
    line(x1 - 12, y1 + 12, x1 + 12, y1 + 12)
    line(x1 - 8, y1 + 18, x1 + 8, y1 + 18)
    line(x1 - 4, y1 + 24, x1 + 4, y1 + 24)
    return parts
  end
  if c.type == "R" then
    if horizontal then
      line(x1, y1, mx - 16, my); line(mx + 16, my, x2, y2)
      parts[#parts + 1] = svg.rect(mx - 16, my - 8, 32, 16, { ["stroke-width"] = 1.5 })
    else
      line(x1, y1, mx, my - 16); line(mx, my + 16, x2, y2)
      parts[#parts + 1] = svg.rect(mx - 8, my - 16, 16, 32, { ["stroke-width"] = 1.5 })
    end
  elseif c.type == "C" then
    if horizontal then
      line(x1, y1, mx - 4, my); line(mx + 4, my, x2, y2)
      line(mx - 4, my - 12, mx - 4, my + 12); line(mx + 4, my - 12, mx + 4, my + 12)
    else
      line(x1, y1, mx, my - 4); line(mx, my + 4, x2, y2)
      line(mx - 12, my - 4, mx + 12, my - 4); line(mx - 12, my + 4, mx + 12, my + 4)
    end
  elseif c.type == "L" then
    if horizontal then
      line(x1, y1, mx - 24, my); line(mx + 24, my, x2, y2)
      parts[#parts + 1] = svg.path(string.format("M %g %g q 6 -14 12 0 q 6 -14 12 0 q 6 -14 12 0 q 6 -14 12 0", mx - 24, my), { ["stroke-width"] = 1.5 })
    else
      line(x1, y1, mx, my - 24); line(mx, my + 24, x2, y2)
      parts[#parts + 1] = svg.path(string.format("M %g %g q 14 6 0 12 q 14 6 0 12 q 14 6 0 12 q 14 6 0 12", mx, my - 24), { ["stroke-width"] = 1.5 })
    end
  elseif c.type == "V" or c.type == "I" then
    -- Leads to the circle, so the source visibly joins its two nodes.
    if horizontal then
      line(x1, y1, mx - 16 * (x2 >= x1 and 1 or -1), my); line(mx + 16 * (x2 >= x1 and 1 or -1), my, x2, y2)
    else
      line(x1, y1, mx, my - 16 * (y2 >= y1 and 1 or -1)); line(mx, my + 16 * (y2 >= y1 and 1 or -1), x2, y2)
    end
    parts[#parts + 1] = svg.circle(mx, my, 16, { ["stroke-width"] = 1.5 })
    -- Unit vector from `from` to `to`; the source raises `to` above `from`.
    local len = math.max(1, math.sqrt((x2 - x1) ^ 2 + (y2 - y1) ^ 2))
    local ux, uy = (x2 - x1) / len, (y2 - y1) / len
    if c.type == "V" then
      text(mx + ux * 8, my + uy * 8 + 4, "+", "middle")
      text(mx - ux * 8, my - uy * 8 + 4, "−", "middle")
    else
      local angle = math.deg(math.atan2(uy, ux))
      parts[#parts + 1] = svg.path(
        string.format("M %g %g l 10 0 m -4 -4 l 4 4 l -4 4", mx - 5, my),
        { ["stroke-width"] = 1.5, transform = string.format("rotate(%g %g %g)", angle, mx, my) }
      )
    end
  elseif c.type == "opamp" then
    -- Triangle between the inputs; − pin above + pin, output at the apex.
    -- Leads run vertically then horizontally from each point to its pin.
    local x0, ym = math.max(x1, x2) + 20, (y1 + y2) / 2
    local minus_y, plus_y, apex_x = ym - 12, ym + 12, x0 + 40
    parts[#parts + 1] = svg.path(string.format("M %g %g l 0 48 l 40 -24 z", x0, ym - 24), { ["stroke-width"] = 1.5 })
    lead(string.format("M %g %g V %g H %g", x1, y1, plus_y, x0))
    lead(string.format("M %g %g V %g H %g", x2, y2, minus_y, x0))
    if x3 then lead(string.format("M %g %g H %g V %g", apex_x, ym, x3, y3)) end
    text(x0 + 4, minus_y + 4, "−", "start")
    text(x0 + 4, plus_y + 4, "+", "start")
    text(x0 + 20, ym - 30, name_of(c), "middle")
  elseif c.type == "probe" then
    local label = c.label or c.id or "probe"
    parts[#parts + 1] = svg.circle(x1, y1, 4, { fill = "currentColor", stroke = "none" })
    local value = result and result.probes and result.probes[label]
    local shown = ""
    if c.hidden then
      shown = ": ?"
    elseif value then
      shown = ": " .. fmt(value, "V")
    end
    text(x1 + 8, y1 - 8, label .. shown, "start")
  end
  if c.type ~= "probe" and c.type ~= "opamp" then
    -- Beside a vertical part, above a horizontal one: never over the symbol.
    if horizontal then
      text(mx, my - 22, label_for(c), "middle")
    else
      text(mx + 24, my + 4, label_for(c), "start")
    end
  end
  return parts
end

local function schematic(components, result)
  local minx, miny, maxx, maxy = 0, 0, 1, 1
  for _, c in ipairs(components) do
    for _, p in ipairs({ c.from, c.to, c.third }) do
      local x, y = pt(p)
      if x then
        minx = math.min(minx, x); maxx = math.max(maxx, x)
        miny = math.min(miny, y); maxy = math.max(maxy, y)
      end
    end
  end
  local width = MARGIN * 2 + (maxx - minx) * GRID + 80
  local height = MARGIN * 2 + (maxy - miny) * GRID + 60
  local body = {}
  for _, c in ipairs(components) do
    if pt(c.from) then
      local single = c.type == "ground" or c.type == "probe" or not pt(c.to)
      local x1, y1 = pos(c.from, minx, miny)
      local x2, y2 = x1, y1
      if not single then x2, y2 = pos(c.to, minx, miny) end
      local x3, y3
      if pt(c.third) then x3, y3 = pos(c.third, minx, miny) end
      for _, el in ipairs(symbols(c, x1, y1, x2, y2, x3, y3, result)) do body[#body + 1] = el end
    end
  end
  return svg.document(width, height, table.concat(body, ""))
end

-- ---- API ------------------------------------------------------------------

local function input_for(args)
  local components = args.components or {}
  local analysis = args.analysis or { dc = true }
  return { components = components, analysis = analysis, title = args.title }
end

local function sorted_lines(map, unit, suffix)
  local lines = {}
  for name, v in pairs(map) do
    lines[#lines + 1] = name .. ((suffix and suffix[name]) or "") .. " = " .. fmt(v, unit)
  end
  table.sort(lines)
  return table.concat(lines, ", ")
end

-- What the learner's side may see: hidden probes keep their name, not their value.
local function public(result)
  local probes = {}
  for name, v in pairs(result.probes) do
    if not result.hidden[name] then probes[name] = v end
  end
  local copy = table.clone(result)
  copy.probes = probes
  return copy
end

-- Model-facing text: probes, node voltages, branch currents, warnings.
local function report(input, result)
  local hidden = {}
  for name in pairs(result.hidden) do hidden[name] = " (hidden from the learner)" end
  local sections = { "Solved (" .. (input.analysis.ac and "AC" or "DC") .. ")." }
  local probes = sorted_lines(result.probes, "V", hidden)
  if probes ~= "" then sections[#sections + 1] = "Probes: " .. probes .. "." end
  local nodes = sorted_lines(result.nodeNames, "V")
  if nodes ~= "" then sections[#sections + 1] = "Nodes (to ground): " .. nodes .. "." end
  local currents = sorted_lines(result.currents, "A")
  if currents ~= "" then sections[#sections + 1] = "Currents (from → to through each part): " .. currents .. "." end
  if #result.warnings > 0 then sections[#sections + 1] = "Warnings: " .. table.concat(result.warnings, "; ") .. "." end
  return table.concat(sections, " ")
end

local function draw(args)
  local input = input_for(args)
  local result = solve_netlist(input)
  local picture = schematic(input.components, result)
  return {
    text = report(input, result),
    artifact = {
      kind = "svg",
      svg = picture,
      data = {
        title = input.title,
        components = input.components,
        analysis = input.analysis,
        result = public(result),
      },
    },
  }
end

local function render(data)
  return schematic(data.components or {}, data.result)
end

-- `answer` = { components, analysis } as the view holds them; the grader
-- re-solves rather than trusting values the view computed.
-- `target` = { probe | component, equals, tolerance? (relative, default 0.05),
-- floor? (absolute, default 1e-6) }. Phasors compare by magnitude.
local function grade(answer, target)
  target = target or {}
  answer = answer or {}
  local components = answer.components or {}
  local want = tonumber(target.equals) or 0
  local tol = math.max((tonumber(target.tolerance) or 0.05) * math.abs(want), tonumber(target.floor) or 1e-6)
  local function close(got)
    if got == nil then return false end
    if type(got) == "table" then return math.abs(num.abs(got) - math.abs(want)) <= tol end
    return math.abs(got - want) <= tol
  end
  if target.probe then
    local result = solve_netlist(input_for(answer))
    local got = result.probes[target.probe]
    local ok = close(got)
    return {
      correct = ok,
      detail = target.probe .. " is " .. (got ~= nil and fmt(got, "V") or "missing") .. "; expected " .. eng(want, "V"),
    }
  end
  if target.component then
    local got
    for _, c in ipairs(components) do
      if c.id == target.component then got = tonumber(c.value) end
    end
    local ok = close(got)
    return {
      correct = ok,
      detail = target.component .. " is " .. (got and tostring(got) or "missing") .. "; expected " .. tostring(want),
    }
  end
  return { correct = false, detail = "the target names neither a probe nor a component" }
end

return {
  circuit_draw = draw,
  circuit_solve = function(args)
    local input = input_for(args)
    return { text = report(input, solve_netlist(input)) }
  end,
  render = render,
  solve = function(args)
    return public(solve_netlist(input_for(args)))
  end,
  grade = grade,
}
