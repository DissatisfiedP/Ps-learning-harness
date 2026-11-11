//! Tool-call arguments as the model streamed them, parsed leniently.
//!
//! Models get two things wrong often enough to break a turn:
//! - a Windows path written unescaped (`{"path": "C:\Users\me\a.pdf"}`): `\U`
//!   is not a JSON escape, so the whole object fails to parse;
//! - no arguments at all for a tool whose fields are all optional (`""`).
//!
//! A string literal holding a backslash that starts no valid escape was
//! written raw: every backslash in it is literal (`\notes` stays `\notes`, not
//! a newline), except `\\` and `\"`. Anything still unparseable is kept as a
//! string so the runtime can answer with an error the model can fix.

use serde_json::Value;

pub(crate) fn parse_tool_args(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::Object(Default::default());
    }
    if let Some(value) = parse(trimmed) {
        return value;
    }
    let repaired = escape_raw_literals(trimmed);
    if repaired != trimmed {
        if let Some(value) = parse(&repaired) {
            return value;
        }
    }
    Value::String(raw.to_owned())
}

/// JSON, with an object encoded twice (`"{\"path\":…}"`) unwrapped once.
fn parse(text: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(text).ok()? {
        Value::String(inner) => match serde_json::from_str::<Value>(&inner) {
            Ok(object @ Value::Object(_)) => Some(object),
            _ => Some(Value::String(inner)),
        },
        value => Some(value),
    }
}

/// Rewrite every string literal that holds an invalid escape so its
/// backslashes are literal; literals with only valid escapes are untouched.
fn escape_raw_literals(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '"' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // A string literal: its end is the first quote not escaped by `\`.
        let start = i + 1;
        let mut end = start;
        while end < chars.len() && chars[end] != '"' {
            end += if chars[end] == '\\' { 2 } else { 1 };
        }
        let end = end.min(chars.len());
        let body = &chars[start..end];
        out.push('"');
        if is_raw(body) {
            let mut j = 0;
            while j < body.len() {
                match (body[j], body.get(j + 1)) {
                    ('\\', Some(&next @ ('\\' | '"'))) => {
                        out.push('\\');
                        out.push(next);
                        j += 2;
                    }
                    ('\\', _) => {
                        out.push_str("\\\\");
                        j += 1;
                    }
                    (c, _) => {
                        out.push(c);
                        j += 1;
                    }
                }
            }
        } else {
            out.extend(body);
        }
        if end < chars.len() {
            out.push('"');
        }
        i = end + 1;
    }
    out
}

/// Whether a literal's body holds a backslash that starts no valid escape.
fn is_raw(body: &[char]) -> bool {
    let mut j = 0;
    while j < body.len() {
        if body[j] != '\\' {
            j += 1;
            continue;
        }
        let valid = match body.get(j + 1) {
            Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => true,
            Some('u') => {
                body.len() >= j + 6 && body[j + 2..j + 6].iter().all(char::is_ascii_hexdigit)
            }
            _ => false,
        };
        if !valid {
            return true;
        }
        j += 2;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_unescaped_windows_path_is_read_as_written() {
        let raw = r#"{"path": "C:\Users\amal\Downloads\unit 3.pdf", "lines": "1-40"}"#;
        assert_eq!(
            parse_tool_args(raw),
            json!({ "path": r"C:\Users\amal\Downloads\unit 3.pdf", "lines": "1-40" })
        );
    }

    #[test]
    fn a_raw_path_keeps_every_backslash_and_other_strings_keep_escapes() {
        // `\U` is no escape, so the literal was written raw: `\n` in `\notes`
        // is part of the path, not a newline.
        let raw = r#"{"path": "C:\Users\me\notes.pdf", "note": "a\tb \u00e9 \"q\""}"#;
        assert_eq!(
            parse_tool_args(raw),
            json!({ "path": r"C:\Users\me\notes.pdf", "note": "a\tb é \"q\"" })
        );
    }

    #[test]
    fn empty_arguments_are_an_empty_object() {
        assert_eq!(parse_tool_args(""), json!({}));
        assert_eq!(parse_tool_args("  "), json!({}));
    }

    #[test]
    fn a_twice_encoded_object_is_unwrapped() {
        assert_eq!(
            parse_tool_args(r#""{\"path\":\"a.pdf\"}""#),
            json!({ "path": "a.pdf" })
        );
    }

    #[test]
    fn a_cut_off_object_stays_a_string() {
        assert_eq!(
            parse_tool_args(r#"{"path": "a.pd"#),
            Value::String(r#"{"path": "a.pd"#.into())
        );
    }
}
