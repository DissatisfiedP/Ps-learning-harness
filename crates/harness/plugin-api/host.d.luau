-- Host API, api 1 (docs/plugins.md "Host API"). Types only.
--
-- Pass this file to `luau-analyze` together with the plugin's src/*.luau
-- (`cargo run -p harness --bin plugin -- test <dir>` does it when the binary
-- is installed). `require("host.<name>")` returns the same table as
-- `host.<name>`; it cannot be typed, so prefer the global.
--
-- Numbers: every `Number` below accepts a plain number (real part, imaginary
-- part 0) or a `Complex` (a table with a numeric `re`; `im` defaults to 0).
-- Any other table is not a number and makes the call an error. Functions
-- return a plain number when |imaginary part| < 1e-12 (so `num.c(3, 0)` is
-- just 3), and a `Complex` otherwise.

type Complex = { re: number, im: number }
type Number = number | Complex

type Matrix = { { Number } }
type Vector = { Number }

type SvgAttributes = { [string]: string | number | boolean }

type Svg = {
    -- A complete document: sets xmlns, width/height/viewBox, fill=none and
    -- stroke=currentColor, so the drawing follows the UI theme.
    document: (width: number, height: number, content: string) -> string,
    group: (attrs: SvgAttributes?, content: string) -> string,
    path: (d: string, attrs: SvgAttributes?) -> string,
    line: (x1: number, y1: number, x2: number, y2: number, attrs: SvgAttributes?) -> string,
    rect: (x: number, y: number, w: number, h: number, attrs: SvgAttributes?) -> string,
    circle: (cx: number, cy: number, r: number, attrs: SvgAttributes?) -> string,
    -- `a` may carry `text-anchor`, `font-size`, `fill`, `stroke`, `class`.
    text: (x: number, y: number, text: string, attrs: SvgAttributes?) -> string,
    escape: (text: string) -> string,
}

type Num = {
    c: (re: number, im: number?) -> Number,
    add: (a: Number, b: Number) -> Number,
    sub: (a: Number, b: Number) -> Number,
    -- matrix × vector → vector, matrix × matrix → matrix, number × number.
    -- Anything else (a number times a matrix, mismatched sizes, ragged rows)
    -- is an error naming the problem ("mul: …").
    mul: ((a: Matrix, b: Vector) -> Vector) & ((a: Matrix, b: Matrix) -> Matrix) & ((a: Number, b: Number) -> Number),
    div: (a: Number, b: Number) -> Number,
    abs: (a: Number) -> number,
    phaseDeg: (a: Number) -> number,
    conj: (a: Number) -> Number,
    real: (a: Number) -> number,
    imag: (a: Number) -> number,
    -- Partial-pivoting LU. Errors ("the matrix is singular…") on no unique
    -- solution; shapes must match. `mul` and `linsolve` refuse matrices or
    -- vectors with more than 400 rows or columns.
    linsolve: (a: Matrix, b: Vector) -> Vector,
}

type Rng = {
    -- [0, 1)
    next: () -> number,
    range: (lo: number, hi: number) -> number,
    -- inclusive
    int: (lo: number, hi: number) -> number,
}

type Host = {
    json: {
        encode: (value: any) -> string,
        decode: (text: string) -> any,
    },
    svg: Svg,
    num: Num,
    -- Deterministic: the seed comes from the caller, never from the clock, so
    -- a plugin's output is reproducible.
    rand: { seeded: (seed: number) -> Rng },
    log: { debug: (value: any) -> () },
    -- Only inside `plugin test` (tests/*.luau); nil at runtime. `golden`
    -- compares `text` with tests/golden/<name> and errors naming the first
    -- differing line; PLUGIN_UPDATE_GOLDEN=1 writes the file instead.
    test: { golden: (name: string, text: string) -> () }?,
}

declare host: Host

-- require("host.json" | "host.svg" | "host.num" | "host.rand" | "host.log")
declare function require(name: string): any
