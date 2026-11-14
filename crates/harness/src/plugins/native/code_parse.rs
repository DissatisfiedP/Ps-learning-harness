//! `code-parse`: real parsing for programming sections (architecture §4.24,
//! docs/plugins.md §12). The tools take the code as text, so the plugin needs
//! no permission: the tutor reads a file with `file_read` and passes it on.
//!
//! - `code_parse_outline`: the definitions (functions, methods, classes,
//!   structs, …) nested, with line ranges and signatures.
//! - `code_parse_errors`: every `ERROR` and `MISSING` node tree-sitter's error
//!   recovery produced, with exact line and column ranges.
//! - `code_parse_locate`: the definitions matching a name or dotted path,
//!   plus the lines that use the name as an identifier.
//!
//! Lines and columns are 1-based; columns count Unicode scalar values, not
//! bytes (tree-sitter's columns are bytes). A `firstLine` argument shifts
//! every line number, for code read as a range of a longer file.
//!
//! The grammar set is fixed and small because each grammar is a compiled C
//! parse table (sizes in docs/plugins.md §12): Python, JavaScript (with JSX),
//! TypeScript, TSX, Rust and C.

use std::fmt::Write as _;
use std::sync::Arc;

use providers::ToolSpec;
use serde_json::Value;
use tree_sitter::{Language, Node, Parser, Tree};

use super::{blocking, done, fail, ready, NativePackage};
use crate::plugins::{Plugin, PluginContext, PluginManifest, ToolFuture};

/// Largest `source` accepted (the same cap as `file_read`).
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
/// Outline entries listed before "… N more".
const MAX_OUTLINE: usize = 300;
/// Syntax errors listed before "… N more".
const MAX_ERRORS: usize = 20;
/// Lines of uses listed by `locate`.
const MAX_USES: usize = 40;
/// Lines of definition text returned by `locate { text: true }`.
const MAX_TEXT_LINES: usize = 200;
/// Characters of a signature or a quoted source line.
const HEADER_CHARS: usize = 100;
const QUOTE_CHARS: usize = 120;
/// Characters of the offending text quoted for an `ERROR` node.
const SNIPPET_CHARS: usize = 40;

/// A built-in grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Rust,
    C,
}

impl Lang {
    /// The `language` argument: a canonical name or a common file extension.
    pub fn from_name(name: &str) -> Result<Self, String> {
        Ok(match name.trim().to_ascii_lowercase().as_str() {
            "python" | "py" => Lang::Python,
            "javascript" | "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "typescript" | "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "rust" | "rs" => Lang::Rust,
            "c" | "h" => Lang::C,
            other => {
                return Err(format!(
                    "`language` `{other}` is not one of the parsed languages: python, javascript, typescript, tsx, rust, c"
                ))
            }
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::Rust => "rust",
            Lang::C => "c",
        }
    }

    fn grammar(self) -> Language {
        match self {
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::C => tree_sitter_c::LANGUAGE.into(),
        }
    }

    fn is_js_family(self) -> bool {
        matches!(self, Lang::JavaScript | Lang::TypeScript | Lang::Tsx)
    }
}

/// A parsed source.
pub struct Parsed<'s> {
    pub lang: Lang,
    pub source: &'s str,
    /// Added to every reported line number (`firstLine - 1`).
    pub line_offset: usize,
    tree: Tree,
}

/// Parse `source` with the grammar for `lang`.
pub fn parse(lang: Lang, source: &str, first_line: usize) -> Result<Parsed<'_>, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "`source` is {} KiB, over the 256 KiB limit; pass one part of the file (and its `firstLine`)",
            source.len() / 1024
        ));
    }
    let mut parser = Parser::new();
    parser
        .set_language(&lang.grammar())
        .map_err(|e| format!("the {} grammar did not load ({e})", lang.name()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "the parser gave up on this source".to_owned())?;
    Ok(Parsed {
        lang,
        source,
        line_offset: first_line.max(1) - 1,
        tree,
    })
}

/// A position: 1-based line (with the offset applied) and 1-based column in
/// characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Parsed<'_> {
    fn pos(&self, byte: usize, point: tree_sitter::Point) -> Pos {
        let line_start = byte - point.column;
        let chars = self
            .source
            .get(line_start..byte)
            .map_or(point.column, |s| s.chars().count());
        Pos {
            line: point.row + 1 + self.line_offset,
            col: chars + 1,
        }
    }

    fn start(&self, node: Node<'_>) -> Pos {
        self.pos(node.start_byte(), node.start_position())
    }

    /// The byte and point of the node's first non-whitespace character (its
    /// start when it is all whitespace).
    fn first_text(&self, node: Node<'_>) -> (usize, tree_sitter::Point) {
        let text = self.text(node);
        let skip = text.len() - text.trim_start().len();
        if skip == text.len() {
            return (node.start_byte(), node.start_position());
        }
        let prefix = &text[..skip];
        let mut point = node.start_position();
        match prefix.rfind('\n') {
            Some(nl) => {
                point.row += prefix.matches('\n').count();
                point.column = skip - nl - 1;
            }
            None => point.column += skip,
        }
        (node.start_byte() + skip, point)
    }

    /// The position just after the node's last character.
    fn end(&self, node: Node<'_>) -> Pos {
        self.pos(node.end_byte(), node.end_position())
    }

    fn text(&self, node: Node<'_>) -> &str {
        self.source
            .get(node.start_byte()..node.end_byte())
            .unwrap_or_default()
    }

    /// Source line `row` (0-based, before the offset), without its newline.
    fn line(&self, row: usize) -> &str {
        self.source
            .split('\n')
            .nth(row)
            .unwrap_or_default()
            .trim_end_matches('\r')
    }

    fn has_errors(&self) -> bool {
        self.tree.root_node().has_error()
    }

    fn line_count(&self) -> usize {
        let n = self.source.lines().count();
        n.max(1)
    }
}

/// `a`–`b` as a span: `line 3, col 5`, `line 3, cols 5–9` or
/// `line 3, col 5 to line 4, col 2` (columns inclusive).
fn span(start: Pos, end: Pos) -> String {
    // `end` is exclusive; the last character is one before it.
    if end.line == start.line {
        let last = end.col.saturating_sub(1);
        if last <= start.col {
            format!("line {}, col {}", start.line, start.col)
        } else {
            format!("line {}, cols {}–{}", start.line, start.col, last)
        }
    } else if end.col == 1 {
        format!(
            "line {}, col {} to the end of line {}",
            start.line,
            start.col,
            end.line - 1
        )
    } else {
        format!(
            "line {}, col {} to line {}, col {}",
            start.line,
            start.col,
            end.line,
            end.col - 1
        )
    }
}

fn lines(start: Pos, end: Pos) -> String {
    // A node ending at the start of a line ends on the line before.
    let last = if end.col == 1 && end.line > start.line {
        end.line - 1
    } else {
        end.line
    };
    if last == start.line {
        format!("line {}", start.line)
    } else {
        format!("lines {}–{}", start.line, last)
    }
}

/// Whitespace runs collapsed to one space, cut to `max` characters with `…`.
fn squeeze(text: &str, max: usize) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    cut(&out, max)
}

fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Definitions

/// One definition found in the tree.
pub struct Def<'t> {
    pub node: Node<'t>,
    /// `function`, `method`, `class`, `struct`, … .
    pub kind: &'static str,
    /// The name used in paths; `None` for anonymous ones.
    pub name: Option<String>,
    /// The node holding the name, so `locate` doesn't count it as a use.
    pub name_node: Option<Node<'t>>,
    /// The signature: the node's text up to its body.
    pub header: String,
    /// Index of the enclosing definition in the list.
    pub parent: Option<usize>,
}

/// Is `node` a definition? `parent_kind` is the enclosing definition's kind.
fn definition<'t>(
    p: &Parsed<'_>,
    node: Node<'t>,
    parent_kind: Option<&str>,
) -> Option<(&'static str, Option<Node<'t>>, Option<Node<'t>>)> {
    // (kind, name node, body node)
    let field = |n: Node<'t>, f: &str| n.child_by_field_name(f);
    let in_type = matches!(parent_kind, Some("class" | "impl" | "trait" | "interface"));
    let function = if in_type { "method" } else { "function" };
    let kind = node.kind();
    match p.lang {
        Lang::Python => match kind {
            "function_definition" => Some((function, field(node, "name"), field(node, "body"))),
            "class_definition" => Some(("class", field(node, "name"), field(node, "body"))),
            _ => None,
        },
        l if l.is_js_family() => match kind {
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                Some(("function", field(node, "name"), field(node, "body")))
            }
            "class_declaration" | "abstract_class_declaration" => {
                Some(("class", field(node, "name"), field(node, "body")))
            }
            "method_definition" | "abstract_method_signature" => {
                Some(("method", field(node, "name"), field(node, "body")))
            }
            "variable_declarator" | "field_definition" | "public_field_definition" => {
                let value = field(node, "value")?;
                if !matches!(
                    value.kind(),
                    "arrow_function" | "function_expression" | "function" | "generator_function"
                ) {
                    return None;
                }
                let name = field(node, "name").or_else(|| field(node, "property"));
                let kind = if kind == "variable_declarator" {
                    "function"
                } else {
                    "method"
                };
                Some((kind, name, field(value, "body")))
            }
            "interface_declaration" => {
                Some(("interface", field(node, "name"), field(node, "body")))
            }
            "type_alias_declaration" => Some(("type", field(node, "name"), None)),
            "enum_declaration" => Some(("enum", field(node, "name"), field(node, "body"))),
            "internal_module" | "module" => {
                Some(("namespace", field(node, "name"), field(node, "body")))
            }
            _ => None,
        },
        Lang::Rust => {
            let kind = match kind {
                "function_item" | "function_signature_item" => {
                    if in_type {
                        "method"
                    } else {
                        "fn"
                    }
                }
                "struct_item" => "struct",
                "enum_item" => "enum",
                "union_item" => "union",
                "trait_item" => "trait",
                "impl_item" => {
                    // Named by its type without generic arguments: `impl<T>
                    // Tr for S<T>` is `S`, so `S.method` finds its methods.
                    let mut ty = field(node, "type")?;
                    while let Some(inner) = field(ty, "type") {
                        ty = inner;
                    }
                    return Some(("impl", Some(ty), field(node, "body")));
                }
                "mod_item" => "mod",
                "type_item" => "type",
                "macro_definition" => "macro",
                _ => return None,
            };
            Some((kind, field(node, "name"), field(node, "body")))
        }
        Lang::C => match kind {
            "function_definition" => {
                // The name is at the end of the declarator chain:
                // pointer_declarator → function_declarator → identifier.
                let mut d = field(node, "declarator");
                while let Some(n) = d {
                    if n.kind().ends_with("identifier") {
                        break;
                    }
                    d = field(n, "declarator").or_else(|| n.named_child(0));
                }
                Some(("function", d, field(node, "body")))
            }
            "struct_specifier" | "union_specifier" | "enum_specifier" => {
                let body = field(node, "body")?;
                let kind = match kind {
                    "struct_specifier" => "struct",
                    "union_specifier" => "union",
                    _ => "enum",
                };
                // `typedef struct { … } T;` is named by the typedef.
                let name = field(node, "name").or_else(|| {
                    node.parent()
                        .filter(|t| t.kind() == "type_definition")
                        .and_then(|t| field(t, "declarator"))
                });
                Some((kind, name, Some(body)))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Where a definition's signature ends: at its body, less any comments
/// between the signature and the body (`def f(x):  # note`).
fn header_end(node: Node<'_>, body: Option<Node<'_>>) -> usize {
    let Some(body) = body else {
        return node.end_byte();
    };
    // A body that isn't a direct child (a JS arrow function's) ends the
    // header at the body itself.
    if body.parent() != Some(node) {
        return body.start_byte();
    }
    let mut end = node.start_byte();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.start_byte() >= body.start_byte() {
            break;
        }
        if !child.kind().contains("comment") {
            end = child.end_byte();
        }
    }
    end
}

/// Every definition, in source order, each pointing at its enclosing one.
pub fn definitions<'t>(p: &'t Parsed<'_>) -> Vec<Def<'t>> {
    let mut out: Vec<Def<'t>> = Vec::new();
    // Explicit stack: deeply nested code must not overflow the thread stack.
    let mut stack: Vec<(Node<'t>, Option<usize>)> = vec![(p.tree.root_node(), None)];
    while let Some((node, parent)) = stack.pop() {
        let parent_kind = parent.map(|i| out[i].kind);
        let mut here = parent;
        if let Some((kind, name_node, body)) = definition(p, node, parent_kind) {
            let end = header_end(node, body);
            let header = squeeze(
                p.source.get(node.start_byte()..end).unwrap_or_default(),
                HEADER_CHARS,
            );
            out.push(Def {
                node,
                kind,
                name: name_node.map(|n| p.text(n).to_owned()),
                name_node,
                header,
                parent,
            });
            here = Some(out.len() - 1);
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, here));
        }
    }
    out
}

/// The names of `defs[i]` and its enclosing definitions, outermost first.
fn path(defs: &[Def<'_>], i: usize) -> Vec<String> {
    let mut names = Vec::new();
    let mut at = Some(i);
    while let Some(j) = at {
        names.push(defs[j].name.clone().unwrap_or_else(|| "(anonymous)".into()));
        at = defs[j].parent;
    }
    names.reverse();
    names
}

fn depth(defs: &[Def<'_>], i: usize) -> usize {
    let mut d = 0;
    let mut at = defs[i].parent;
    while let Some(j) = at {
        d += 1;
        at = defs[j].parent;
    }
    d
}

fn parse_note(p: &Parsed<'_>) -> String {
    if p.has_errors() {
        let n = syntax_errors(p).len();
        format!(
            "{n} syntax error{} (code_parse_errors gives their positions; definitions near them may be missing)",
            if n == 1 { "" } else { "s" }
        )
    } else {
        "parses cleanly".into()
    }
}

/// `code_parse_outline`.
pub fn outline(p: &Parsed<'_>) -> String {
    let defs = definitions(p);
    let mut out = format!(
        "{}, {} line{}, {}.\n",
        p.lang.name(),
        p.line_count(),
        if p.line_count() == 1 { "" } else { "s" },
        parse_note(p)
    );
    if defs.is_empty() {
        out.push_str("No definitions (functions, classes, types, …) at any level.");
        return out;
    }
    for (i, def) in defs.iter().enumerate().take(MAX_OUTLINE) {
        let _ = writeln!(
            out,
            "{}{} ({}): {}",
            "  ".repeat(depth(&defs, i)),
            lines(p.start(def.node), p.end(def.node)),
            def.kind,
            def.header
        );
    }
    if defs.len() > MAX_OUTLINE {
        let _ = writeln!(
            out,
            "… {} more; outline a part of the file to see them",
            defs.len() - MAX_OUTLINE
        );
    }
    out.truncate(out.trim_end().len());
    out
}

// ---------------------------------------------------------------------------
// Syntax errors

/// One syntax error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub start: Pos,
    /// Exclusive end.
    pub end: Pos,
    /// What went wrong, e.g. "missing `)`" or "unexpected `+ ;`".
    pub message: String,
    /// 0-based row of `start` in the source (for quoting).
    row: usize,
}

/// The innermost `ERROR` nodes and every `MISSING` node, in source order.
/// Error recovery can wrap a whole file in one `ERROR` around a nested one
/// that marks the real place, so an `ERROR` that contains another error is
/// opened rather than reported.
pub fn syntax_errors(p: &Parsed<'_>) -> Vec<SyntaxError> {
    let mut out = Vec::new();
    let mut stack = vec![p.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_missing() {
            let what = if node.is_named() {
                node.kind().replace('_', " ")
            } else {
                format!("`{}`", node.kind())
            };
            out.push(SyntaxError {
                start: p.start(node),
                end: p.end(node),
                message: format!("missing {what}{}", context(node)),
                row: node.start_position().row,
            });
            continue;
        }
        let mut cursor = node.walk();
        let nested = node.is_error() && node.children(&mut cursor).any(|c| c.has_error());
        if node.is_error() && !nested {
            let text = p.text(node);
            let message = if text.trim().is_empty() {
                format!("cannot parse here{}", context(node))
            } else {
                format!(
                    "unexpected `{}`{}",
                    squeeze(text, SNIPPET_CHARS),
                    context(node)
                )
            };
            // Recovery often starts an ERROR at the whitespace before the
            // offending text; point at the text itself.
            let (byte, point) = p.first_text(node);
            out.push(SyntaxError {
                start: p.pos(byte, point),
                end: p.end(node),
                message,
                row: point.row,
            });
            continue;
        }
        if !node.has_error() {
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

/// ` in <construct>` for the nearest named ancestor that isn't the file.
fn context(node: Node<'_>) -> String {
    let mut at = node.parent();
    while let Some(n) = at {
        if n.parent().is_none() {
            return String::new();
        }
        if n.is_named() && !n.is_error() {
            return format!(" in {}", n.kind().replace('_', " "));
        }
        at = n.parent();
    }
    String::new()
}

/// The source line of `row`, with a caret line under `col`; long lines are
/// shown as a window around the column. Tabs become one space so the caret
/// stays aligned.
fn quote(p: &Parsed<'_>, row: usize, col: usize) -> String {
    let line: Vec<char> = p
        .line(row)
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .collect();
    let from = if line.len() > QUOTE_CHARS {
        col.saturating_sub(QUOTE_CHARS / 2)
            .min(line.len() - QUOTE_CHARS)
    } else {
        0
    };
    let shown: String = line.iter().skip(from).take(QUOTE_CHARS).collect();
    let caret = col.saturating_sub(1).saturating_sub(from);
    format!("    {}\n    {}^", shown.trim_end(), " ".repeat(caret))
}

/// `code_parse_errors`.
pub fn errors(p: &Parsed<'_>) -> String {
    let errs = syntax_errors(p);
    if errs.is_empty() {
        if p.has_errors() {
            // Defensive: has_error without a reachable node shouldn't happen.
            return format!(
                "{}: the parser reports an error it could not place.",
                p.lang.name()
            );
        }
        return format!(
            "{}: parses cleanly ({} line{}). Parsing checks syntax only, not names, types or behaviour.",
            p.lang.name(),
            p.line_count(),
            if p.line_count() == 1 { "" } else { "s" }
        );
    }
    let mut out = format!(
        "{}: {} syntax error{}. A position is where the parser stopped fitting the code to the grammar; the mistake can be just before it.\n",
        p.lang.name(),
        errs.len(),
        if errs.len() == 1 { "" } else { "s" }
    );
    for (i, e) in errs.iter().enumerate().take(MAX_ERRORS) {
        let _ = writeln!(out, "{}. {}: {}", i + 1, span(e.start, e.end), e.message);
        let _ = writeln!(out, "{}", quote(p, e.row, e.start.col));
    }
    if errs.len() > MAX_ERRORS {
        let _ = writeln!(out, "… {} more after these", errs.len() - MAX_ERRORS);
    }
    out.truncate(out.trim_end().len());
    out
}

// ---------------------------------------------------------------------------
// Locate

/// `code_parse_locate`.
pub fn locate(p: &Parsed<'_>, symbol: &str, with_text: bool) -> Result<String, String> {
    let wanted: Vec<&str> = symbol
        .split("::")
        .flat_map(|s| s.split('.'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let Some(&name) = wanted.last() else {
        return Err(
            "`symbol` is required (a name such as `push` or a path such as `Stack.push`)".into(),
        );
    };
    let defs = definitions(p);
    let matches: Vec<usize> = (0..defs.len())
        .filter(|&i| {
            let full = path(&defs, i);
            full.len() >= wanted.len()
                && full[full.len() - wanted.len()..]
                    .iter()
                    .zip(&wanted)
                    .all(|(a, b)| a == b)
        })
        .collect();

    let mut out = String::new();
    if matches.is_empty() {
        let mut names: Vec<String> = defs.iter().filter_map(|d| d.name.clone()).collect();
        names.sort();
        names.dedup();
        let _ = write!(
            out,
            "No definition of `{symbol}` in this {} source",
            p.lang.name()
        );
        if names.is_empty() {
            out.push_str(" (it defines nothing).\n");
        } else {
            let more = names.len().saturating_sub(40);
            names.truncate(40);
            let _ = writeln!(
                out,
                "; it defines: {}{}.",
                names.join(", "),
                if more > 0 {
                    format!(", … {more} more")
                } else {
                    String::new()
                }
            );
        }
    } else {
        let _ = writeln!(
            out,
            "`{symbol}` in this {} source: {} definition{}.",
            p.lang.name(),
            matches.len(),
            if matches.len() == 1 { "" } else { "s" }
        );
    }
    for &i in &matches {
        let def = &defs[i];
        let (start, end) = (p.start(def.node), p.end(def.node));
        let _ = writeln!(
            out,
            "{} {} — {} ({})\n  {}",
            def.kind,
            path(&defs, i).join("."),
            lines(start, end),
            span(start, end),
            def.header
        );
        if with_text {
            let text = p.text(def.node);
            let total = text.lines().count();
            let shown: Vec<&str> = text.lines().take(MAX_TEXT_LINES).collect();
            // The first line starts at the node, so indent it like the file.
            let indent = " ".repeat(start.col - 1);
            let _ = writeln!(out, "```\n{indent}{}\n```", shown.join("\n"));
            if total > MAX_TEXT_LINES {
                let _ = writeln!(
                    out,
                    "[{} more lines; read {} with file_read]",
                    total - MAX_TEXT_LINES,
                    lines(start, end)
                );
            }
        }
    }

    // Uses: identifier leaves spelled `name`, other than a definition's name.
    let defined: Vec<(usize, usize)> = defs
        .iter()
        .filter_map(|d| d.name_node.map(|n| (n.start_byte(), n.end_byte())))
        .collect();
    let mut rows: Vec<usize> = Vec::new();
    let mut stack = vec![p.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.named_child_count() == 0 {
            if node.kind().ends_with("identifier")
                && p.text(node) == name
                && !defined.contains(&(node.start_byte(), node.end_byte()))
            {
                let row = node.start_position().row;
                if rows.last() != Some(&row) {
                    rows.push(row);
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    if rows.is_empty() {
        let _ = write!(out, "No uses of `{name}` as an identifier.");
    } else {
        let _ = writeln!(
            out,
            "Uses of `{name}` ({} line{}):",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        );
        for &row in rows.iter().take(MAX_USES) {
            let _ = writeln!(
                out,
                "  {}: {}",
                row + 1 + p.line_offset,
                cut(p.line(row).trim(), QUOTE_CHARS)
            );
        }
        if rows.len() > MAX_USES {
            let _ = writeln!(out, "  … {} more lines", rows.len() - MAX_USES);
        }
    }
    out.truncate(out.trim_end().len());
    Ok(out)
}

// ---------------------------------------------------------------------------
// Plugin

/// The built-in plugin.
pub struct CodeParse {
    package: NativePackage,
}

impl CodeParse {
    pub fn new() -> Self {
        let package = NativePackage::parse(
            include_str!("code-parse/plugin.toml"),
            &[
                (
                    "prompts/tutor.md",
                    include_str!("code-parse/prompts/tutor.md"),
                ),
                (
                    "prompts/practice.md",
                    include_str!("code-parse/prompts/practice.md"),
                ),
                (
                    "schemas/code_parse_outline.json",
                    include_str!("code-parse/schemas/code_parse_outline.json"),
                ),
                (
                    "schemas/code_parse_errors.json",
                    include_str!("code-parse/schemas/code_parse_errors.json"),
                ),
                (
                    "schemas/code_parse_locate.json",
                    include_str!("code-parse/schemas/code_parse_locate.json"),
                ),
            ],
        )
        .expect("the code-parse package is valid");
        Self { package }
    }
}

impl Default for CodeParse {
    fn default() -> Self {
        Self::new()
    }
}

/// Run one tool entry on its arguments.
pub fn run(entry: &str, args: &Value) -> Result<String, String> {
    let lang = Lang::from_name(
        args.get("language")
            .and_then(Value::as_str)
            .ok_or("`language` is required (python, javascript, typescript, tsx, rust or c)")?,
    )?;
    let source = args.get("source").and_then(Value::as_str).ok_or(
        "`source` is required: the code itself, not a path (read a file with file_read first)",
    )?;
    let first_line = match args.get("firstLine") {
        None | Some(Value::Null) => 1,
        Some(v) => v
            .as_u64()
            .filter(|&n| n >= 1)
            .ok_or("`firstLine` must be a whole number, 1 or more")? as usize,
    };
    let parsed = parse(lang, source, first_line)?;
    match entry {
        "outline" => Ok(outline(&parsed)),
        "errors" => Ok(errors(&parsed)),
        "locate" => {
            let symbol = args.get("symbol").and_then(Value::as_str).unwrap_or("");
            let with_text = args.get("text").and_then(Value::as_bool).unwrap_or(false);
            locate(&parsed, symbol, with_text)
        }
        other => Err(format!("unknown entry `{other}`")),
    }
}

impl Plugin for CodeParse {
    fn manifest(&self) -> &PluginManifest {
        &self.package.manifest
    }

    fn tools(&self) -> Vec<ToolSpec> {
        self.package.tools.clone()
    }

    fn fragment(&self, agent: &str) -> String {
        self.package.fragment(agent)
    }

    fn call(self: Arc<Self>, name: &str, args: &Value, _ctx: PluginContext) -> ToolFuture {
        let Some(entry) = self.package.entry(name).map(str::to_owned) else {
            return ready(fail(format!("unknown plugin tool `{name}`")));
        };
        let args = args.clone();
        blocking(move || match run(&entry, &args) {
            Ok(text) => done(text),
            Err(e) => fail(e),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parsed(lang: Lang, src: &str) -> Parsed<'_> {
        parse(lang, src, 1).unwrap()
    }

    #[test]
    fn package_loads_with_three_tools_and_no_permissions() {
        let plugin = CodeParse::new();
        let names: Vec<String> = plugin.tools().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "code_parse_outline",
                "code_parse_errors",
                "code_parse_locate"
            ]
        );
        assert!(plugin.manifest().permissions.is_empty());
        assert!(plugin.fragment("tutor").contains("code_parse_errors"));
        assert!(plugin.fragment("practice").contains("code_parse_errors"));
    }

    const PY: &str = "\
import os

class Stack:
    def __init__(self):
        self.items = []

    def push(self, x):
        self.items.append(x)

def push(stack, x):
    # push is also a comment word
    stack.push(x)
    return \"push\"
";

    #[test]
    fn python_outline_nests_methods_with_line_ranges() {
        let out = outline(&parsed(Lang::Python, PY));
        assert_eq!(
            out,
            "python, 13 lines, parses cleanly.\n\
             lines 3–8 (class): class Stack:\n  \
             lines 4–5 (method): def __init__(self):\n  \
             lines 7–8 (method): def push(self, x):\n\
             lines 10–13 (function): def push(stack, x):"
        );
    }

    #[test]
    fn locate_follows_paths_and_skips_strings_and_comments() {
        let p = parsed(Lang::Python, PY);
        let out = locate(&p, "Stack.push", true).unwrap();
        assert!(
            out.starts_with("`Stack.push` in this python source: 1 definition.\n"),
            "{out}"
        );
        assert!(
            out.contains("method Stack.push — lines 7–8 (line 7, col 5 to line 8, col 28)"),
            "{out}"
        );
        assert!(
            out.contains("```\n    def push(self, x):\n        self.items.append(x)\n```"),
            "{out}"
        );
        // Both definitions match the bare name; line 12 is the only use
        // (the comment and the string on lines 11 and 13 are not).
        let bare = locate(&p, "push", false).unwrap();
        assert!(bare.contains("2 definitions"), "{bare}");
        assert!(bare.contains("function push — lines 10–13"), "{bare}");
        assert!(
            bare.ends_with("Uses of `push` (1 line):\n  12: stack.push(x)"),
            "{bare}"
        );
        // `firstLine` shifts every reported line.
        let shifted = locate(&parse(Lang::Python, PY, 101).unwrap(), "Stack.push", false).unwrap();
        assert!(shifted.contains("lines 107–108"), "{shifted}");
        assert!(shifted.contains("  112: stack.push(x)"), "{shifted}");

        let none = locate(&p, "Queue.pop", false).unwrap();
        assert!(
            none.starts_with("No definition of `Queue.pop` in this python source; it defines: Stack, __init__, push."),
            "{none}"
        );
        assert!(locate(&p, " .. ", false).is_err());
    }

    #[test]
    fn js_missing_paren_is_a_zero_width_error_at_the_right_column() {
        let src = "function f(a) {\n  return g(a, 1;\n}\n";
        let p = parsed(Lang::JavaScript, src);
        let errs = syntax_errors(&p);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].message, "missing `)` in arguments");
        assert_eq!(errs[0].start, Pos { line: 2, col: 16 });
        assert_eq!(errs[0].end, errs[0].start);
        let out = errors(&p);
        assert!(
            out.contains("1. line 2, col 16: missing `)` in arguments"),
            "{out}"
        );
        assert!(
            out.contains("\n      return g(a, 1;\n                   ^"),
            "{out}"
        );
    }

    #[test]
    fn error_columns_count_characters_not_bytes() {
        // "é" and "ü" are two bytes each: tree-sitter's byte column is 2 more
        // than the character column of the stray `)`.
        let src = "let s = \"éü\");\n";
        let p = parsed(Lang::JavaScript, src);
        let errs = syntax_errors(&p);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].start, Pos { line: 1, col: 13 });
        assert!(errs[0].message.starts_with("unexpected `)`"), "{errs:?}");
    }

    #[test]
    fn python_errors_point_inside_the_construct_not_at_the_whole_file() {
        // The missing `:` after `def g(self)` makes error recovery wrap the
        // whole file in one ERROR; the nested ERROR is the useful place.
        let src = "def f(x):\n    return x +* 2\n\nclass A:\n    def g(self)\n        return 1\n";
        let p = parsed(Lang::Python, src);
        let errs = syntax_errors(&p);
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert_eq!(
            (errs[0].start, errs[0].end),
            (Pos { line: 2, col: 5 }, Pos { line: 2, col: 18 }),
            "{errs:?}"
        );
        // The ERROR starts at the newline after `class A:`; the report starts
        // at its text.
        assert_eq!(
            (errs[1].start, errs[1].end),
            (Pos { line: 5, col: 5 }, Pos { line: 5, col: 16 }),
            "{errs:?}"
        );
        let out = errors(&p);
        assert!(
            out.contains(
                "2. line 5, cols 5–15: unexpected `def g(self)` in class definition\n        def g(self)\n        ^"
            ),
            "{out}"
        );
        // A valid file reports no errors.
        assert!(errors(&parsed(Lang::Python, PY)).starts_with("python: parses cleanly (13 lines)."));
    }

    #[test]
    fn rust_impl_methods_are_found_under_the_type() {
        let src = "mod shapes {\n    pub struct Circle { r: f64 }\n    impl<T> Area for Circle<T> {\n        fn area(&self) -> f64 { 3.14 * self.r * self.r }\n    }\n    trait Area { fn area(&self) -> f64; }\n}\nmacro_rules! sq { ($x:expr) => { $x * $x }; }\n";
        let p = parsed(Lang::Rust, src);
        let out = outline(&p);
        assert!(out.contains("lines 1–7 (mod): mod shapes"), "{out}");
        assert!(
            out.contains("\n  lines 3–5 (impl): impl<T> Area for Circle<T>"),
            "{out}"
        );
        assert!(
            out.contains("\n    line 4 (method): fn area(&self) -> f64"),
            "{out}"
        );
        assert!(
            out.contains("\n    line 6 (method): fn area(&self) -> f64;"),
            "{out}"
        );
        assert!(out.contains("line 8 (macro): macro_rules! sq"), "{out}");
        let found = locate(&p, "shapes::Circle::area", false).unwrap();
        assert!(found.contains("1 definition"), "{found}");
        assert!(
            found.contains("method shapes.Circle.area — line 4"),
            "{found}"
        );
    }

    #[test]
    fn c_functions_are_named_through_their_declarators() {
        let src = "typedef struct { int x; } Point;\nstatic int *make(int n) { return 0; }\nstruct node { struct node *next; };\n";
        let out = outline(&parsed(Lang::C, src));
        assert!(out.contains("line 1 (struct): struct"), "{out}");
        assert!(
            out.contains("line 2 (function): static int *make(int n)"),
            "{out}"
        );
        assert!(out.contains("line 3 (struct): struct node"), "{out}");
        let p = parsed(Lang::C, src);
        assert!(locate(&p, "Point", false)
            .unwrap()
            .contains("struct Point — line 1"));
        assert!(locate(&p, "make", false)
            .unwrap()
            .contains("function make — line 2"));
    }

    #[test]
    fn typescript_and_tsx_definitions() {
        let src = "interface Shape { area(): number }\ntype Id = string;\nexport const area = (s: Shape): number => s.area();\nabstract class Base { abstract run(): void; }\n";
        let out = outline(&parsed(Lang::TypeScript, src));
        assert!(out.contains("line 1 (interface): interface Shape"), "{out}");
        assert!(out.contains("line 2 (type): type Id = string;"), "{out}");
        assert!(
            out.contains("line 3 (function): area = (s: Shape): number =>"),
            "{out}"
        );
        assert!(
            out.contains("  line 4 (method): abstract run(): void"),
            "{out}"
        );
        let tsx = "export function App(): JSX.Element { return <div>{1}</div>; }\n";
        assert!(errors(&parsed(Lang::Tsx, tsx)).contains("parses cleanly"));
        // The same JSX is a syntax error to the plain TypeScript grammar.
        assert!(!errors(&parsed(Lang::TypeScript, tsx)).contains("parses cleanly"));
    }

    #[test]
    fn arguments_are_checked() {
        let err = run("outline", &json!({ "language": "cobol", "source": "x" })).unwrap_err();
        assert!(err.contains("not one of the parsed languages"), "{err}");
        assert!(run("outline", &json!({ "language": "py" }))
            .unwrap_err()
            .contains("`source` is required"));
        let big = "x = 1\n".repeat(MAX_SOURCE_BYTES / 6 + 1);
        let err = run("errors", &json!({ "language": "python", "source": big })).unwrap_err();
        assert!(err.contains("over the 256 KiB limit"), "{err}");
        let err = run(
            "errors",
            &json!({ "language": "rs", "source": "", "firstLine": 0 }),
        )
        .unwrap_err();
        assert!(err.contains("`firstLine`"), "{err}");
        let empty = run("outline", &json!({ "language": "rs", "source": "" })).unwrap();
        assert!(empty.contains("No definitions"), "{empty}");
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let src = format!("x = {}1{}\n", "(".repeat(20_000), ")".repeat(20_000));
        let p = parsed(Lang::Python, &src);
        assert!(outline(&p).contains("No definitions"));
        assert!(errors(&p).contains("parses cleanly"));
        assert!(locate(&p, "x", false)
            .unwrap()
            .contains("Uses of `x` (1 line)"));
    }
}
