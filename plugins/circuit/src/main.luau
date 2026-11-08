--!strict
-- Entry module: the functions plugin.toml names. Tools `circuit_draw` and
-- `circuit_solve`, the renderer `render`, the view's `solve` and `render`,
-- and the answer-mode `grade`.
local num = require("@harness/num")
local model = require("./model")
local solver = require("./solver")
local report = require("./report")
local schematic = require("./schematic")
local grade = require("./grade")

-- An artifact's `data` as stored, or as the view sends it back.
type Figure = {
  components: { model.Part }?,
  result: { probes: { [string]: num.Number }? }?,
}

local function circuitDraw(request: model.Request)
  local circuit = model.circuit(request)
  local solution = solver.solve(circuit)
  return {
    text = report.text(circuit.analysis, solution),
    artifact = {
      kind = "svg",
      svg = schematic.draw(circuit.components, solution.probes),
      data = {
        title = circuit.title,
        components = circuit.components,
        analysis = circuit.analysis,
        result = model.public(solution),
      },
    },
  }
end

local function circuitSolve(request: model.Request)
  local circuit = model.circuit(request)
  return { text = report.text(circuit.analysis, solver.solve(circuit)) }
end

local function render(figure: Figure): string
  local result = figure.result
  return schematic.draw(figure.components or {}, if result then result.probes else nil)
end

local function solve(request: model.Request): model.Solution
  return model.public(solver.solve(model.circuit(request)))
end

return {
  circuit_draw = circuitDraw,
  circuit_solve = circuitSolve,
  render = render,
  solve = solve,
  grade = grade,
}
