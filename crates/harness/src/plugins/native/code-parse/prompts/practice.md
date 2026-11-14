## Code structure

The `code_parse_*` tools parse Python, JavaScript (JSX included), TypeScript, TSX, Rust and C. Pass the code as `source`.

- **Error-reading items.** Run `code_parse_errors` on the broken snippet before you pose it, so the answer key names the exact line and column where the parser fails; check the learner's code the same way when they say it is fixed. The reported position can sit after the real mistake (a missing `:` or `)` often surfaces on the next line); accept an answer that names the real mistake.
- **Read items.** Use `code_parse_outline` or `code_parse_locate` to pick a function from a longer file and to quote its exact text and line range in the task.
- A clean parse is not a correct answer: these tools never run code or check types.
