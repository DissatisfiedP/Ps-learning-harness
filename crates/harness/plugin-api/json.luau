--[=[
	`require("@harness/json")`, api 1 (docs/plugins.md §4.1). Types only: the
	harness supplies the real module at runtime.
]=]

local json = {}

--[=[
	Compact JSON. A non-empty table whose keys are all positive integers
	encodes as an array; any other table encodes as an object (string keys
	only, sorted); functions and other unsupported values become `null`.
]=]
function json.encode(value: any): string
	return nil :: any
end

--[=[
	Parses JSON; invalid JSON raises an error (`invalid JSON: …`).
]=]
function json.decode(text: string): any
	return nil :: any
end

return json
