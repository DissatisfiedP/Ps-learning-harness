--[=[
	`require("@harness/json")`, api 1 (docs/plugins.md §4.1). Types only: the
	harness supplies the real module at runtime.
]=]

local json = {}

--[=[
	Compact JSON. A table whose keys are exactly the integers 1..n encodes
	as an array; any other table encodes as an object (integer keys written
	as strings, keys sorted). An empty table is `{}`, never `[]`. Functions
	and other unsupported values become `null`.
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
