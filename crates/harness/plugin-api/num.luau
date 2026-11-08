--[=[
	`require("@harness/num")`, api 1 (docs/plugins.md §4.3). Types only: the
	harness supplies the real module at runtime.

	Numbers: every `Number` below accepts a plain number (real part, imaginary
	part 0) or a `Complex` (a table with a numeric `re`; `im` defaults to 0).
	Any other table is not a number and makes the call an error. Functions
	return a plain number when |imaginary part| < 1e-12 (so `num.c(3, 0)` is
	just 3), and a `Complex` otherwise.
]=]

export type Complex = { re: number, im: number }
export type Number = number | Complex

export type Matrix = { { Number } }
export type Vector = { Number }

local num = {}

function num.c(re: number, im: number?): Number
	return nil :: any
end

function num.add(a: Number, b: Number): Number
	return nil :: any
end

function num.sub(a: Number, b: Number): Number
	return nil :: any
end

--[=[
	matrix × vector → vector, matrix × matrix → matrix, number × number.
	Anything else (a number times a matrix, mismatched sizes, ragged rows)
	is an error naming the problem ("mul: …").
]=]
num.mul = (nil :: any) :: ((a: Matrix, b: Vector) -> Vector)
	& ((a: Matrix, b: Matrix) -> Matrix)
	& ((a: Number, b: Number) -> Number)

function num.div(a: Number, b: Number): Number
	return nil :: any
end

function num.abs(a: Number): number
	return nil :: any
end

function num.phaseDeg(a: Number): number
	return nil :: any
end

function num.conj(a: Number): Number
	return nil :: any
end

function num.real(a: Number): number
	return nil :: any
end

function num.imag(a: Number): number
	return nil :: any
end

--[=[
	Partial-pivoting LU. Errors ("the matrix is singular…") on no unique
	solution; shapes must match. `mul` and `linsolve` refuse matrices or
	vectors with more than 400 rows or columns.
]=]
function num.linsolve(a: Matrix, b: Vector): Vector
	return nil :: any
end

return num
