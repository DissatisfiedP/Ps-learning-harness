//! The problem checker (architecture §4.11 steps 5–6, `prompts/jobs/problem-
//! checker.md`): a separate agent, model role Planner, that reads a committed
//! problem **before it knows the target**.
//!
//! One conversation, two turns. Turn 1 (`audit_submit`) sees only the
//! statement, so its guess of which technique applies can't be coloured by
//! what the coach meant. Only after that guess is submitted does turn 2
//! (`necessity_submit`) name the target and ask whether the problem can be
//! solved without it. The prompt file holds both turns; the target's name is
//! in turn 2's half only, so the system prompt (sent on both turns) is cut
//! at the `## Turn 2` heading and the second half is posted as the user
//! message after the first answer.
//!
//! [`judge`] turns the two answers into a verdict, in code:
//! - named at once → the problem is at most cue distance 1;
//! - solvable without the target → rejected (for a distractor the opposite:
//!   rejected when it *does* need the target).

use std::collections::HashSet;
use std::sync::Arc;

use providers::{ContentBlock, Message, Role, StopReason};
use serde_json::Value;

use crate::agent::{refuse, result_op, tool_op, Exec};
use crate::jobs::assistant_message;
use crate::prompts::{render, Composer, TemplateValues};
use crate::{Error, Harness, ModelRole};

/// Where the prompt file's second turn begins.
const TURN_2: &str = "\n## Turn 2";
/// Model calls one check may use (the nudges included).
const MAX_STEPS: usize = 6;

/// What the checker needs to know about the problem.
pub struct Request {
    pub statement: String,
    /// The target's title, and the names it is also known by.
    pub target_name: String,
    pub target_equivalents: String,
    pub held_summary: String,
    pub is_distractor: bool,
    pub cue_distance: u8,
}

/// The checker's two answers.
#[derive(Debug, Clone)]
pub struct Report {
    pub audit: Value,
    pub necessity: Value,
}

/// The verdict on a problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judgement {
    /// Go ahead; the cue distance is lowered to `cue_cap` when set.
    Accept { cue_cap: Option<u8> },
    /// Back to the coach, privately.
    Reject(String),
}

/// Lower-case words of a name without articles: `The Product Rule` and
/// `product rule` compare equal.
fn words(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.split_whitespace()
        .filter(|w| !matches!(*w, "the" | "a" | "an"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a technique name from the checker is the target (or something
/// that contains it, or that it contains): `Product rule` for `The product
/// rule of differentiation`.
fn is_target(technique: &str, names: &[String]) -> bool {
    let t = words(technique);
    !t.is_empty()
        && names.iter().map(|n| words(n)).any(|n| {
            !n.is_empty()
                && (t == n
                    || (n.len() >= 4 && t.contains(&n))
                    // A shortened name of two or more words, not one word of it.
                    || (t.contains(' ') && n.contains(&t)))
        })
}

/// Did turn 1 name the target on sight? One of its techniques, held with
/// confidence 0.5 or more, is the target, and the checker said the answer was
/// obvious after the first sentence or the whole statement.
pub fn named_immediately(audit: &Value, names: &[String]) -> bool {
    let obvious = matches!(
        audit.get("obviousAfter").and_then(Value::as_str),
        Some("first sentence" | "whole statement")
    );
    obvious
        && audit
            .get("techniques")
            .and_then(Value::as_array)
            .is_some_and(|ts| {
                ts.iter().any(|t| {
                    t.get("confidence").and_then(Value::as_f64).unwrap_or(0.0) >= 0.5
                        && t.get("name")
                            .and_then(Value::as_str)
                            .is_some_and(|n| is_target(n, names))
                })
            })
}

/// The verdict on a problem from the checker's two answers. `names` are the
/// target's names (title and equivalents).
pub fn judge(
    audit: &Value,
    necessity: &Value,
    names: &[String],
    target: &str,
    is_distractor: bool,
    cue: u8,
) -> Judgement {
    let solved_without = necessity
        .get("solvedWithout")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let route = necessity
        .get("route")
        .and_then(Value::as_str)
        .unwrap_or("(no route given)");
    let first_move = audit
        .get("firstMove")
        .and_then(Value::as_str)
        .unwrap_or("(none)");
    if is_distractor {
        return if solved_without {
            Judgement::Accept { cue_cap: None }
        } else {
            Judgement::Reject(format!(
                "Distractor rejected: the checker could not solve it without {target} (it needs the target), so it isn't a distractor. Its first move was: {first_move}. Write a problem that looks as if it might need {target} but doesn't, or write it again as a normal problem."
            ))
        };
    }
    if solved_without {
        return Judgement::Reject(format!(
            "Rejected: the checker solved it without {target}. Route: {route}. A problem that can be solved that way trains that route, not {target}: change it so {target} is necessary (or write a new problem)."
        ));
    }
    if cue > 1 && named_immediately(audit, names) {
        return Judgement::Accept { cue_cap: Some(1) };
    }
    Judgement::Accept { cue_cap: None }
}

/// The two halves of the prompt file: turn 1 (the system prompt) and turn 2
/// (posted after `audit_submit`).
fn split(prompt: &str) -> Result<(&str, &str), Error> {
    let at = prompt.find(TURN_2).ok_or_else(|| {
        Error::Prompt("jobs/problem-checker.md has no `## Turn 2` heading".into())
    })?;
    Ok((&prompt[..at], &prompt[at + 1..]))
}

/// Render a half with sentinels for the values, then put the values in: a
/// statement may contain `{{`, which `render` would refuse.
fn fill(half: &str, values: &[(&str, &str)]) -> Result<String, Error> {
    let mut v = TemplateValues::new();
    for (key, _) in values {
        v.set(*key, format!("\u{1}{key}\u{1}"));
    }
    let mut text = render(half, &v, &HashSet::new())?;
    for (key, value) in values {
        text = text.replace(&format!("\u{1}{key}\u{1}"), value);
    }
    Ok(text)
}

fn audit_problem(args: &Value) -> Option<String> {
    let techniques = args.get("techniques").and_then(Value::as_array);
    let Some(techniques) = techniques.filter(|t| !t.is_empty() && t.len() <= 5) else {
        return Some("`techniques` is 1 to 5 entries `{ name, confidence, cuedBy }`".into());
    };
    for t in techniques {
        let ok = t
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| !n.trim().is_empty())
            && t.get("confidence")
                .and_then(Value::as_f64)
                .is_some_and(|c| (0.0..=1.0).contains(&c));
        if !ok {
            return Some("every technique needs a `name` and a `confidence` from 0 to 1".into());
        }
    }
    if args
        .get("firstMove")
        .and_then(Value::as_str)
        .is_none_or(|m| m.trim().is_empty())
    {
        return Some("`firstMove` is required".into());
    }
    if !matches!(
        args.get("obviousAfter").and_then(Value::as_str),
        Some("first sentence" | "whole statement" | "partial work" | "not obvious")
    ) {
        return Some(
            "`obviousAfter` is one of: first sentence, whole statement, partial work, not obvious"
                .into(),
        );
    }
    None
}

fn necessity_problem(args: &Value) -> Option<String> {
    if args.get("solvedWithout").and_then(Value::as_bool).is_none() {
        return Some("`solvedWithout` (true or false) is required".into());
    }
    if args
        .get("route")
        .and_then(Value::as_str)
        .is_none_or(|r| r.trim().is_empty())
    {
        return Some("`route` is required: how you solved it, or why you could not".into());
    }
    None
}

impl Harness {
    /// Run the two-turn check on one problem. Planner role; one at a time.
    /// `Err` when the checker never produced both answers.
    pub(crate) async fn run_checker(
        self: &Arc<Self>,
        session_id: &str,
        req: &Request,
    ) -> Result<Report, Error> {
        let composer = Composer::new(&self.repo);
        let file = composer.load("jobs/problem-checker.md")?;
        let (turn1, turn2) = split(&file)?;
        let system = fill(turn1, &[("statement", &req.statement)])?;
        let second = fill(
            turn2,
            &[
                ("target_name", &req.target_name),
                ("target_equivalents", &req.target_equivalents),
                ("held_summary", &req.held_summary),
            ],
        )?;
        let audit_tool = crate::tools::checker_audit_tool();
        let necessity_tool = crate::tools::checker_necessity_tool();

        let _slot = Arc::clone(&self.planner_slots).acquire_owned().await;
        let cache_key = format!("{session_id}-checker-{}", crate::sessions::op_id());
        let mut messages = vec![Message::text(Role::User, "Begin turn 1.")];
        let mut audit: Option<Value> = None;
        let mut necessity: Option<Value> = None;
        for _ in 0..MAX_STEPS {
            let (tool, other) = if audit.is_none() {
                (&audit_tool, &necessity_tool)
            } else {
                (&necessity_tool, &audit_tool)
            };
            let reply = self
                .call_role(
                    ModelRole::Planner,
                    session_id,
                    &cache_key,
                    &system,
                    &messages,
                    std::slice::from_ref(tool),
                    None,
                    None,
                )
                .await?;
            let calls = reply.completion.tool_calls.clone();
            if reply.completion.stop == StopReason::Refusal {
                return Err(Error::Invalid("the problem checker refused".into()));
            }
            messages.push(assistant_message(
                &reply.text,
                &reply.completion.native,
                &calls,
            ));
            if calls.is_empty() {
                messages.push(Message::text(
                    Role::User,
                    format!("Answer with {}.", tool.name),
                ));
                continue;
            }
            let mut results: Vec<ContentBlock> = Vec::new();
            for c in &calls {
                self.practice_append(session_id, tool_op(c, Some("checker")), true)?;
                let exec = if c.name == other.name {
                    refuse(&c.name, "not available at this point of the check")
                } else if c.name != tool.name {
                    refuse(&c.name, "unknown tool")
                } else {
                    let problem = if audit.is_none() {
                        audit_problem(&c.arguments)
                    } else {
                        necessity_problem(&c.arguments)
                    };
                    match problem {
                        Some(why) => refuse(&c.name, why),
                        None => {
                            if audit.is_none() {
                                audit = Some(c.arguments.clone());
                                crate::agent::done("Recorded.")
                            } else if necessity.is_none() {
                                necessity = Some(c.arguments.clone());
                                crate::agent::done("Recorded.")
                            } else {
                                refuse(&c.name, "the check is complete")
                            }
                        }
                    }
                };
                let Exec::Done { content, is_error } = exec else {
                    continue;
                };
                self.practice_append(
                    session_id,
                    result_op(c, &content, is_error, Some("checker")),
                    true,
                )?;
                results.push(ContentBlock::ToolResult {
                    call_id: c.id.clone(),
                    content,
                    is_error,
                });
            }
            if let (Some(audit), Some(necessity)) = (&audit, &necessity) {
                return Ok(Report {
                    audit: audit.clone(),
                    necessity: necessity.clone(),
                });
            }
            // The first answer is in: now, and only now, turn 2.
            if audit.is_some() && messages.iter().all(|m| !is_turn_2(m, &second)) {
                results.push(ContentBlock::text(second.clone()));
            }
            messages.push(Message {
                role: Role::User,
                content: results,
            });
        }
        Err(Error::Invalid(
            "the problem checker did not answer both turns".into(),
        ))
    }
}

fn is_turn_2(m: &Message, second: &str) -> bool {
    m.content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { text } if text == second))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The arguments of an `audit_submit` call.
    fn sample_audit(first: &str, obvious: &str, technique: &str, confidence: f64) -> Value {
        json!({
            "techniques": [{ "name": technique, "confidence": confidence, "cuedBy": "wording" }],
            "firstMove": first,
            "obviousAfter": obvious
        })
    }

    fn names() -> Vec<String> {
        vec!["The derivative".into(), "differentiation".into()]
    }

    #[test]
    fn the_prompt_file_splits_before_the_targets_name_appears() {
        let file = Composer::bundled("jobs/problem-checker.md").unwrap();
        let (one, two) = split(file).unwrap();
        assert!(one.contains("{{statement}}"));
        assert!(
            !one.contains("target_name") && !one.contains("{{held_summary}}"),
            "turn 1 can't hold what turn 2 reveals"
        );
        assert!(two.starts_with("## Turn 2") && two.contains("{{target_name}}"));
        let system = fill(one, &[("statement", "Use {{braces}} freely")]).unwrap();
        assert!(system.contains("Use {{braces}} freely"));
    }

    #[test]
    fn naming_the_target_on_sight_caps_the_cue_distance_at_one() {
        let audit = sample_audit("differentiate", "first sentence", "Derivative", 0.9);
        let none = json!({ "solvedWithout": false, "route": "no route" });
        let ok = |a: &Value, cue| judge(a, &none, &names(), "derivative", false, cue);
        assert_eq!(ok(&audit, 3), Judgement::Accept { cue_cap: Some(1) });
        assert_eq!(ok(&audit, 1), Judgement::Accept { cue_cap: None });
        // Obvious only after partial work, or held with low confidence: no cap.
        let late = sample_audit("set up h(t)", "partial work", "Derivative", 0.9);
        assert_eq!(ok(&late, 3), Judgement::Accept { cue_cap: None });
        let unsure = sample_audit("differentiate", "whole statement", "Derivative", 0.3);
        assert_eq!(ok(&unsure, 3), Judgement::Accept { cue_cap: None });
        let other = sample_audit("integrate", "first sentence", "Integration", 0.9);
        assert_eq!(ok(&other, 3), Judgement::Accept { cue_cap: None });
    }

    #[test]
    fn a_problem_that_can_be_solved_without_the_target_is_rejected_with_the_route() {
        let audit = sample_audit("read the table", "not obvious", "Lookup", 0.8);
        let without = json!({ "solvedWithout": true, "route": "the answer is in the table" });
        let Judgement::Reject(why) = judge(&audit, &without, &names(), "derivative", false, 2)
        else {
            panic!("should reject");
        };
        assert!(
            why.contains("the answer is in the table") && why.contains("derivative"),
            "{why}"
        );
    }

    #[test]
    fn a_distractor_is_rejected_when_it_does_need_the_target() {
        let audit = sample_audit("differentiate", "partial work", "Derivative", 0.8);
        let needs = json!({ "solvedWithout": false, "route": "could not" });
        let Judgement::Reject(why) = judge(&audit, &needs, &names(), "derivative", true, 2) else {
            panic!("should reject");
        };
        assert!(why.contains("Distractor rejected"), "{why}");
        let free = json!({ "solvedWithout": true, "route": "constant speed, no calculus" });
        assert_eq!(
            judge(&audit, &free, &names(), "derivative", true, 3),
            Judgement::Accept { cue_cap: None },
            "a distractor is never capped"
        );
    }

    #[test]
    fn technique_names_match_loosely_but_not_by_accident() {
        let n = vec!["The product rule".to_owned()];
        assert!(is_target("product rule", &n));
        assert!(is_target("Product Rule (differentiation)", &n));
        assert!(!is_target("chain rule", &n));
        assert!(!is_target("", &n));
        assert!(
            !is_target("rule", &n),
            "a 4-letter fragment of someone else's name"
        );
    }
}
