//! The answer check by code (architecture §4.11 step 7): run the coach's
//! script and compare what it prints with the committed `finalAnswer`.
//!
//! The script runs under the CAS engine (`python3` with SymPy preloaded,
//! [`cas::check`]); where SymPy is missing but `python3` isn't, the same
//! script runs as plain Python, so a script that doesn't need SymPy still
//! counts. When neither can run the check is **skipped**, not failed: the
//! problem goes ahead unverified and the coach is told so.
//!
//! Comparison, in order, over the script's last printed line:
//! 1. the same text (whitespace and case aside);
//! 2. a printed number: it must agree with a number in the committed answer
//!    at the precision that answer was written to (`2.04` accepts
//!    `2.0408…`; integers must agree exactly; thousands separators and
//!    spaces inside a number are ignored);
//! 3. SymPy's decision for symbolic answers ([`cas::equivalent`]).

use crate::plugins::native::cas::{self, CasError, Equivalence};
use crate::plugins::native::exec::{self, Spec};

use super::CheckScript;

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// The script reproduced the committed answer.
    Passed,
    /// It printed something else.
    Mismatch { script: String },
    /// The script crashed, timed out or printed nothing: the message says how.
    ScriptFailed(String),
    /// This device can't run it (python3 or SymPy is missing).
    Skipped(String),
}

/// Run `check` and compare with `final_answer`. Blocking: call it from
/// `spawn_blocking`.
pub fn check_answer(check: &CheckScript, final_answer: &str) -> Check {
    let printed = match run_script(&check.code) {
        Ok(printed) => printed,
        Err(done) => return done,
    };
    let Some(last) = printed
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_owned)
    else {
        return Check::ScriptFailed(
            "the script printed nothing: print the final answer on its last line".into(),
        );
    };
    if same_text(&last, final_answer) || same_number(&last, final_answer) {
        return Check::Passed;
    }
    // A symbolic answer: let SymPy decide. A side it can't parse is not equal.
    let (a, b) = (rhs(&last), rhs(final_answer));
    match cas::equivalent(a, b) {
        Ok(Equivalence::Equal) => Check::Passed,
        Ok(_) => Check::Mismatch { script: last },
        Err(CasError::Unavailable(_) | CasError::Failed(_)) => Check::Mismatch { script: last },
    }
}

/// What the script printed, or the finished verdict when it can't run.
fn run_script(code: &str) -> Result<String, Check> {
    match cas::check(code) {
        Ok(out) if out.ok => Ok(out.output),
        Ok(out) => Err(Check::ScriptFailed(format!(
            "the script failed:\n{}",
            exec::clip(&out.output, 1200)
        ))),
        Err(CasError::Failed(why)) => Err(Check::ScriptFailed(why)),
        Err(CasError::Unavailable(why)) => run_plain(code, why),
    }
}

/// SymPy is missing (or python3 is): the script may not need it.
fn run_plain(code: &str, why: String) -> Result<String, Check> {
    let Some(python) = exec::find_any(&["python3", "python"]) else {
        return Err(Check::Skipped(why));
    };
    let spec = Spec::new(python)
        .args(["-I", "check.py"])
        .timeout(cas::TIMEOUT)
        .output_cap(cas::OUTPUT_CAP)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONIOENCODING", "utf-8");
    let out = exec::run(&spec, &[("check.py", code)], "").map_err(Check::ScriptFailed)?;
    if out.success() {
        return Ok(out.stdout);
    }
    let all = out.combined();
    if all.contains("No module named") {
        // It does need a module this device doesn't have.
        Err(Check::Skipped(why))
    } else {
        Err(Check::ScriptFailed(format!(
            "the script failed:\n{}",
            exec::clip(&all, 1200)
        )))
    }
}

fn squash(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn same_text(a: &str, b: &str) -> bool {
    squash(a) == squash(b)
}

/// The part after the last `=`: `x = 150` compares as `150`.
fn rhs(s: &str) -> &str {
    s.rsplit('=').next().unwrap_or(s).trim()
}

/// A number written in the text, and how many decimals it was written with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Num {
    value: f64,
    decimals: usize,
}

/// Every number in `s`. A space or comma between digit groups of three is a
/// thousands separator (`22 500`, `1,000,000`); a dot is the decimal point.
pub(crate) fn numbers(s: &str) -> Vec<Num> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts =
            c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit));
        if !starts {
            i += 1;
            continue;
        }
        let negative = i > 0
            && matches!(chars[i - 1], '-' | '−')
            && !chars[..i - 1]
                .last()
                .is_some_and(|p| p.is_alphanumeric() || *p == ')');
        let mut text = String::new();
        let mut decimals = 0;
        let mut seen_dot = false;
        // Digits since the last thousands separator, and whether one was seen.
        let mut group = 0;
        let mut grouped = false;
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_digit() {
                text.push(c);
                group += 1;
                if seen_dot {
                    decimals += 1;
                }
            } else if c == '.' && !seen_dot && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                seen_dot = true;
                text.push('.');
            } else if !seen_dot
                && matches!(c, ' ' | ',' | '\u{a0}' | '\u{202f}' | '\u{2009}')
                && (if grouped {
                    group == 3
                } else {
                    (1..=3).contains(&group)
                })
                && chars
                    .get(i + 1..i + 4)
                    .is_some_and(|g| g.iter().all(char::is_ascii_digit))
                && !chars.get(i + 4).is_some_and(char::is_ascii_digit)
            {
                // A thousands separator: the next group is exactly three digits.
                grouped = true;
                group = 0;
            } else {
                break;
            }
            i += 1;
        }
        // Scientific notation: 1.5e-3.
        if matches!(chars.get(i), Some('e' | 'E')) {
            let mut j = i + 1;
            if matches!(chars.get(j), Some('+' | '-')) {
                j += 1;
            }
            if chars.get(j).is_some_and(char::is_ascii_digit) {
                let mut exp = String::from("e");
                if let Some(sign @ ('+' | '-')) = chars.get(i + 1) {
                    exp.push(*sign);
                }
                while chars.get(j).is_some_and(char::is_ascii_digit) {
                    exp.push(chars[j]);
                    j += 1;
                }
                text.push_str(&exp);
                i = j;
                decimals = 0;
            }
        }
        if let Ok(v) = text.parse::<f64>() {
            out.push(Num {
                value: if negative { -v } else { v },
                decimals,
            });
        }
    }
    out
}

/// Whether two written numbers are the same number at the coarser of their
/// precisions: `2.04` and `2.0408` are; `150` and `150.3` are not.
fn agree(a: Num, b: Num) -> bool {
    let diff = (a.value - b.value).abs();
    let scale = a.value.abs().max(b.value.abs()).max(1.0);
    match a.decimals.min(b.decimals) {
        0 if a.decimals == 0 && b.decimals == 0 => diff <= 1e-9 * scale,
        0 => diff <= 1e-6 * scale,
        d => diff <= 0.5 * 10f64.powi(-(d as i32)) + 1e-9,
    }
}

/// The script printed one number: does the committed answer contain it?
fn same_number(script: &str, committed: &str) -> bool {
    let printed = numbers(script);
    let [only] = printed.as_slice() else {
        return false;
    };
    // Only a line that is nothing but that number counts: `x+3` is symbolic.
    let bare = script.trim().trim_start_matches(['-', '−', '+']);
    if !bare
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
    {
        return false;
    }
    numbers(committed).into_iter().any(|n| agree(*only, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nums(s: &str) -> Vec<f64> {
        numbers(s).into_iter().map(|n| n.value).collect()
    }

    #[test]
    fn numbers_read_separators_signs_decimals_and_exponents() {
        assert_eq!(nums("22 500 plots"), [22500.0]);
        assert_eq!(nums("1,000,000"), [1_000_000.0]);
        assert_eq!(nums("t ≈ 2.04 s"), [2.04]);
        assert_eq!(nums("x = -3, y = 4"), [-3.0, 4.0]);
        assert_eq!(nums("1.5e-3 A"), [0.0015]);
        assert_eq!(
            nums("2 500 and 12"),
            [2500.0, 12.0],
            "a group of two digits isn't a thousands group"
        );
        assert_eq!(
            nums("A(x)=x(600-2x)"),
            [600.0, 2.0],
            "an operator is not a sign"
        );
        assert_eq!(numbers("2.04")[0].decimals, 2);
    }

    #[test]
    fn a_printed_number_matches_the_committed_one_at_the_committed_precision() {
        assert!(same_number("2.0408163", "t ≈ 2.04 s"));
        assert!(same_number("22500", "22 500 plots"));
        assert!(same_number("150", "x = 150 m"));
        assert!(
            !same_number("150.3", "x = 150 m"),
            "an integer answer is exact"
        );
        assert!(!same_number("2.0408163", "t ≈ 2.05 s"));
        assert!(!same_number("22500", "22 600 plots"));
        // Two numbers, or an expression, are not a printed number.
        assert!(!same_number("3, 4", "3"));
        assert!(!same_number("x + 3", "3 - x"));
        assert!(!same_number("3 - x", "3"));
    }

    #[test]
    fn text_matches_ignore_spacing_and_case() {
        assert!(same_text("Both  loops", "both loops"));
        assert!(!same_text("both loops", "one loop"));
        assert_eq!(rhs("x = 150"), "150");
        assert_eq!(rhs("2*x + 3"), "2*x + 3");
    }

    #[test]
    fn a_script_with_nothing_to_run_it_with_is_skipped_not_failed() {
        // The environment may or may not have python3; either way the result
        // is one of the three honest outcomes, never a panic or a mismatch
        // about an answer nobody computed.
        let check = CheckScript {
            language: "python".into(),
            code: "print(2.5 * 2)".into(),
        };
        match check_answer(&check, "5") {
            Check::Passed | Check::Skipped(_) => {}
            other => panic!("{other:?}"),
        }
    }
}
