--[=[
	`require("@harness/test")`, api 1 (docs/plugins.md §9). Types only: the
	harness supplies the real module.

	Only inside `plugin test` (tests/*.luau and what they require); requiring
	it in a running plugin is an error.
]=]

local test = {}

--[=[
	Compares `text` with tests/golden/<name> and errors naming the first
	differing line; PLUGIN_UPDATE_GOLDEN=1 writes the file instead.
]=]
function test.golden(name: string, text: string): () end

return test
