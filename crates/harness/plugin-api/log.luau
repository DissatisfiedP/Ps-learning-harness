--[=[
	`require("@harness/log")`, api 1 (docs/plugins.md §4.5). Types only: the
	harness supplies the real module at runtime.
]=]

local log = {}

--[=[
	Writes `value` as JSON, one line, to the app's `tracing` log (target
	`plugin`). Never shown to the model or the learner.
]=]
function log.debug(value: any): () end

return log
