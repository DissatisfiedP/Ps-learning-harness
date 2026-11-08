-- Run with `cargo run -p harness --bin plugin -- test plugins/fixtures/ipc-probe`.
local api = require("../src/main")

return {
  show_returns_an_svg_artifact = function()
    local out = api.show({})
    assert(out.artifact.kind == "svg", "kind")
    assert(string.find(out.artifact.svg, "<svg", 1, true) == 1, out.artifact.svg)
  end,
}
