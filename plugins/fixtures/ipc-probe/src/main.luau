-- The probe's only tool draws a placeholder figure; the work happens in
-- views/probe.html, which runs in the sandboxed view frame.
local svg = require("@harness/svg")

local function render(_data)
  return svg.document(240, 40, svg.text(8, 24, "IPC escape probe (open the view)"))
end

local function show(_args)
  return {
    text = "IPC probe ready.",
    artifact = { kind = "svg", svg = render({}), data = {} },
  }
end

return {
  show = show,
  render = render,
}
