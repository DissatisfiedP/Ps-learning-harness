--!strict
-- Entry module: the functions plugin.toml names. Tools `circuit_draw` and
-- `circuit_solve`, the renderer `render`, the view's `solve` and `render`,
-- and the answer-mode `grade`.
local model = require("./model")
local solver = require("./solver")
local report = require("./report")
local schematic = require("./schematic")
local grade = require("./grade")

-- An artifact's `data` as stored, or as the view sends it back: the request
-- that drew it and the learner-safe solution.
type Figure = model.Request & { result: model.Solution? }

local function circuitDraw(request: model.Request)
  local circuit = model.circuit(request)
  local solved = solver.solve(circuit)
  -- The text first: it may add warnings, which the figure's data then carries.
  local text = report.text(circuit, solved)
  local shown = model.public(solved.solution)
  return {
    text = text,
    artifact = {
      kind = "svg",
      svg = schematic.draw(circuit, shown),
      data = {
        title = circuit.title,
        components = circuit.components,
        analysis = circuit.analysis,
        show = circuit.show,
        annotations = circuit.annotations,
        result = shown,
      },
    },
  }
end

local function circuitSolve(request: model.Request)
  local circuit = model.circuit(request)
  return { text = report.text(circuit, solver.solve(circuit)) }
end

local function render(figure: Figure): string
  return schematic.draw(model.circuit(figure), figure.result)
end

local function solve(request: model.Request): model.Solution
  return model.public(solver.solve(model.circuit(request)).solution)
end

return {
  circuit_draw = circuitDraw,
  circuit_solve = circuitSolve,
  render = render,
  solve = solve,
  grade = grade,
}
