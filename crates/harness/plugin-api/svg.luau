--[=[
	`require("@harness/svg")`, api 1 (docs/plugins.md §4.2). Types only: the
	harness supplies the real module at runtime.

	Strings are escaped; attributes are emitted in sorted key order, so the
	output is deterministic.
]=]

export type Attributes = { [string]: string | number | boolean }

local svg = {}

--[=[
	A complete document: sets xmlns, width/height/viewBox, fill=none and
	stroke=currentColor, so the drawing follows the UI theme.
]=]
function svg.document(width: number, height: number, content: string): string
	return nil :: any
end

function svg.group(attrs: Attributes?, content: string): string
	return nil :: any
end

function svg.path(d: string, attrs: Attributes?): string
	return nil :: any
end

function svg.line(x1: number, y1: number, x2: number, y2: number, attrs: Attributes?): string
	return nil :: any
end

function svg.rect(x: number, y: number, w: number, h: number, attrs: Attributes?): string
	return nil :: any
end

function svg.circle(cx: number, cy: number, r: number, attrs: Attributes?): string
	return nil :: any
end

--[=[
	`attrs` may carry `text-anchor`, `font-size`, `fill`, `stroke`, `class`.
]=]
function svg.text(x: number, y: number, text: string, attrs: Attributes?): string
	return nil :: any
end

function svg.escape(text: string): string
	return nil :: any
end

return svg
