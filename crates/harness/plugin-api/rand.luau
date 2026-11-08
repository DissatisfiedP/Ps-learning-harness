--[=[
	`require("@harness/rand")`, api 1 (docs/plugins.md §4.4). Types only: the
	harness supplies the real module at runtime.
]=]

export type Rng = {
	-- [0, 1)
	next: () -> number,
	range: (lo: number, hi: number) -> number,
	-- inclusive
	int: (lo: number, hi: number) -> number,
}

local rand = {}

--[=[
	Deterministic: the seed comes from the caller, never from the clock, so
	a plugin's output is reproducible.
]=]
function rand.seeded(seed: number): Rng
	return nil :: any
end

return rand
