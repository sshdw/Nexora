//! Agent action memory (Layer 2): what prior runs EXECUTED.
//!
//! Layer 1 (`history`) carries conversation TEXT into the next run, but not
//! what the agent did: which files it read/wrote, which commands it ran,
//! with what result. That trace already exists in `agent_steps`
//! (`tool_name`, raw JSON `arguments`, `observation`, `status`); this module
//! compresses the last few runs' traces into short per-call summary lines.
//!
//! Form decision (fixed): the summary is appended to the SYSTEM prompt next
//! to the Layer-1 truncation note. Real `assistant(tool_calls)` +
//! `Tool(result)` messages are never replayed: strict pairing plus call ids,
//! they would evict the conversation inside the message window, and pairing
//! skew is a provider 400.
//!
//! Budgets (fixed): the last [`MAX_PRIOR_RUNS`] runs, at most
//! [`MAX_ACTION_LINES`] lines, each line at most [`MAX_LINE_CHARS`]
//! characters. `denied` steps are included (a user refusal is a strong
//! signal). This module never imports the repository layer: the call site
//! maps `AgentStep` rows onto [`AgentStepView`].

use std::fmt::Write as _;

/// How many prior runs contribute to the trace (newest first).
pub(crate) const MAX_PRIOR_RUNS: usize = 3;

/// Cap on emitted trace lines across all walked runs.
pub(crate) const MAX_ACTION_LINES: usize = 30;

/// Cap on characters per emitted line, including the truncation marker.
pub(crate) const MAX_LINE_CHARS: usize = 200;

/// One persisted step as needed by the summarizer: the tool identity, its
/// raw arguments, its outcome, and its observation. Built at the call site
/// from `AgentStep` rows so this module stays repository-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentStepView {
    /// Tool name (`tool_name`), empty for `model_turn` steps.
    pub tool_name: String,
    /// Raw JSON arguments exactly as provider-supplied (`arguments`).
    pub arguments: String,
    /// Tool output / denial text / approval decision (`observation`).
    pub observation: String,
    /// Tool call outcome (`status`).
    pub status: String,
}

/// The compressed trace: one line per emittable step plus budget leftovers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionSummary {
    /// One `run <id>: <tool>(<args>) -> <status> (<obs>)` line per step,
    /// newest runs first, `seq` order within a run.
    pub lines: Vec<String>,
    /// Runs beyond [`MAX_PRIOR_RUNS`] that were never walked.
    pub omitted_runs: usize,
    /// Emittable steps never emitted for any budget reason.
    pub omitted_steps: usize,
}

/// Compress per-run step lists into an [`ActionSummary`].
///
/// `steps_by_run` is `(run_id, steps)` newest-first with steps in `seq`
/// order (exactly what `list_runs_by_conversation` + `list_steps` yield).
/// Steps without a tool name are skipped: they are `model_turn` narration,
/// i.e. conversation text Layer 1 already covers. `tool_call` and `approval`
/// steps are kept with ALL statuses, including `denied` and `cancelled`.
/// Runs beyond [`MAX_PRIOR_RUNS`] count into `omitted_runs` (their emittable
/// steps also count into `omitted_steps`, since that text is equally absent
/// from context); steps past [`MAX_ACTION_LINES`] count into
/// `omitted_steps`.
pub(crate) fn summarize(steps_by_run: &[(i64, Vec<AgentStepView>)]) -> ActionSummary {
    let mut lines = Vec::new();
    let mut omitted_runs = 0;
    let mut omitted_steps = 0;
    let mut walked = 0;
    for (run_id, steps) in steps_by_run {
        if walked >= MAX_PRIOR_RUNS {
            omitted_runs += 1;
            omitted_steps += steps
                .iter()
                .filter(|step| !step.tool_name.is_empty())
                .count();
            continue;
        }
        walked += 1;
        for step in steps {
            if step.tool_name.is_empty() {
                continue;
            }
            if lines.len() >= MAX_ACTION_LINES {
                omitted_steps += 1;
            } else {
                lines.push(format_line(*run_id, step));
            }
        }
    }
    ActionSummary {
        lines,
        omitted_runs,
        omitted_steps,
    }
}

/// Render the summary as a system-prompt block, or `None` when there is
/// nothing to say. The block opens with
/// `Prior action trace (N calls across M prior run(s)):`, lists the lines,
/// and — when any run or step was omitted — closes with the exact tail
/// marker `...(<omitted> more steps omitted)`. Bounded by construction:
/// at most [`MAX_ACTION_LINES`] lines of at most [`MAX_LINE_CHARS`]
/// characters (~6KB total).
pub(crate) fn system_note(summary: &ActionSummary) -> Option<String> {
    if summary.lines.is_empty() {
        return None;
    }
    let mut block = format!(
        "Prior action trace ({} calls across {} prior run(s)):",
        summary.lines.len(),
        contributing_runs(&summary.lines),
    );
    for line in &summary.lines {
        block.push('\n');
        block.push_str(line);
    }
    if summary.omitted_runs > 0 || summary.omitted_steps > 0 {
        block.push('\n');
        let _ = write!(block, "...({} more steps omitted)", summary.omitted_steps);
    }
    Some(block)
}

/// How many distinct runs contributed lines: the distinct `run <id>:` line
/// prefixes `summarize` emits.
fn contributing_runs(lines: &[String]) -> usize {
    let mut ids: Vec<i64> = Vec::new();
    for line in lines {
        let Some(rest) = line.strip_prefix("run ") else {
            continue;
        };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() || !rest[digits.len()..].starts_with(':') {
            continue;
        }
        if let Ok(id) = digits.parse::<i64>() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids.len()
}

/// One `run <id>: <tool>(<args>) -> <status> (<obs>)` line, truncated to
/// [`MAX_LINE_CHARS`] characters for the whole line.
fn format_line(run_id: i64, step: &AgentStepView) -> String {
    let status = if step.status.is_empty() {
        "unknown"
    } else {
        step.status.as_str()
    };
    truncate_chars(
        &format!(
            "run {run_id}: {}({}) -> {status} ({})",
            step.tool_name,
            short_args(&step.arguments),
            first_line(&step.observation),
        ),
        MAX_LINE_CHARS,
    )
}

/// The identifying scalar of a raw provider-arguments JSON string: the
/// `path`, `command`, then `content` value (first hit wins; non-string
/// scalars render as JSON, missing values fall through). Anything else —
/// invalid JSON, non-objects, no usable key — is the raw string itself.
/// Always reduced to one line so the per-step line shape holds.
fn short_args(arguments: &str) -> String {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(arguments)
    {
        for key in ["path", "command", "content"] {
            match map.get(key) {
                Some(serde_json::Value::String(value)) if !value.is_empty() => {
                    return first_line(value)
                }
                Some(value) if value.is_number() || value.is_boolean() => return value.to_string(),
                _ => {}
            }
        }
    }
    first_line(arguments)
}

/// First line of `text`, trimmed. Keeps multi-line observations and contents
/// from breaking the one-line-per-step shape.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

/// Truncate to at most `max` characters total, appending `...` when
/// anything was cut. Char-boundary safe: counts and cuts on `char`s, so
/// multi-byte text is never split mid-code-point.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{kept}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(tool: &str, arguments: &str, observation: &str, status: &str) -> AgentStepView {
        AgentStepView {
            tool_name: tool.to_string(),
            arguments: arguments.to_string(),
            observation: observation.to_string(),
            status: status.to_string(),
        }
    }

    fn model_turn(narration: &str) -> AgentStepView {
        AgentStepView {
            tool_name: String::new(),
            arguments: String::new(),
            observation: narration.to_string(),
            status: String::new(),
        }
    }

    #[test]
    fn line_shape_extracts_path_over_other_keys() {
        let summary = summarize(&[(
            7,
            vec![view(
                "read_file",
                r#"{"path": "notes.txt", "command": "dir", "content": "body"}"#,
                "file output",
                "succeeded",
            )],
        )]);
        assert_eq!(
            summary.lines,
            vec!["run 7: read_file(notes.txt) -> succeeded (file output)"]
        );
        assert_eq!(summary.omitted_runs, 0);
        assert_eq!(summary.omitted_steps, 0);
    }

    #[test]
    fn line_shape_extracts_command_when_no_path() {
        let summary = summarize(&[(
            3,
            vec![view(
                "execute_command",
                r#"{"command": "dir", "content": "body"}"#,
                "listing",
                "succeeded",
            )],
        )]);
        assert_eq!(
            summary.lines,
            vec!["run 3: execute_command(dir) -> succeeded (listing)"]
        );
    }

    #[test]
    fn line_shape_extracts_content_when_no_path_or_command() {
        let summary = summarize(&[(
            3,
            vec![view(
                "write_file",
                r#"{"path2": "miss", "content": "hello"}"#,
                "diff",
                "succeeded",
            )],
        )]);
        assert_eq!(
            summary.lines,
            vec!["run 3: write_file(hello) -> succeeded (diff)"]
        );
    }

    #[test]
    fn line_shape_falls_back_to_raw_arguments() {
        let plain = summarize(&[(
            1,
            vec![view("read_file", "not json at all", "out", "succeeded")],
        )]);
        assert_eq!(
            plain.lines,
            vec!["run 1: read_file(not json at all) -> succeeded (out)"]
        );
        let array = summarize(&[(
            1,
            vec![view("read_file", r#"["a", "b"]"#, "out", "succeeded")],
        )]);
        assert_eq!(
            array.lines,
            vec![r#"run 1: read_file(["a", "b"]) -> succeeded (out)"#]
        );
    }

    #[test]
    fn denied_and_cancelled_steps_are_kept() {
        let summary = summarize(&[(
            9,
            vec![
                view(
                    "write_file",
                    r#"{"path": "secret.txt"}"#,
                    "denied",
                    "denied",
                ),
                view(
                    "execute_command",
                    r#"{"command": "sleep"}"#,
                    "cancelled by the user",
                    "cancelled",
                ),
            ],
        )]);
        assert_eq!(summary.lines.len(), 2);
        assert!(summary.lines[0].contains("denied"), "{}", summary.lines[0]);
        assert!(
            summary.lines[1].contains("cancelled"),
            "{}",
            summary.lines[1]
        );
    }

    #[test]
    fn model_turn_steps_without_tool_name_are_skipped() {
        let summary = summarize(&[(
            5,
            vec![
                model_turn("the agent said hello"),
                view(
                    "read_file",
                    r#"{"path": "a.txt"}"#,
                    "content here",
                    "succeeded",
                ),
                model_turn("final answer"),
            ],
        )]);
        assert_eq!(
            summary.lines,
            vec!["run 5: read_file(a.txt) -> succeeded (content here)"]
        );
    }

    #[test]
    fn observation_keeps_only_its_first_line() {
        let summary = summarize(&[(
            2,
            vec![view(
                "read_file",
                r#"{"path": "a.txt"}"#,
                "first line\nsecond line\nthird",
                "succeeded",
            )],
        )]);
        assert_eq!(
            summary.lines,
            vec!["run 2: read_file(a.txt) -> succeeded (first line)"]
        );
    }

    #[test]
    fn per_line_truncation_caps_at_200_chars_with_ellipsis() {
        let long_obs = "o".repeat(500);
        let summary = summarize(&[(
            4,
            vec![view(
                "read_file",
                r#"{"path": "a.txt"}"#,
                &long_obs,
                "succeeded",
            )],
        )]);
        assert_eq!(summary.lines.len(), 1);
        let line = &summary.lines[0];
        assert!(line.chars().count() <= MAX_LINE_CHARS, "{line}");
        assert!(line.ends_with("..."), "{line}");
        // Char-boundary safety: multi-byte text in the cut zone is never split.
        let emoji_obs = "💾".repeat(20) + &"e".repeat(170);
        let emoji = summarize(&[(
            4,
            vec![view(
                "read_file",
                r#"{"path": "a.txt"}"#,
                &emoji_obs,
                "succeeded",
            )],
        )]);
        let emoji_line = &emoji.lines[0];
        assert!(emoji_line.chars().count() <= MAX_LINE_CHARS, "{emoji_line}");
        assert!(emoji_line.ends_with("..."), "{emoji_line}");
        assert!(emoji_line.contains('💾'), "{emoji_line}");
    }

    #[test]
    fn newest_runs_first_with_seq_order_within_a_run() {
        let summary = summarize(&[
            (
                11,
                vec![
                    view("read_file", r#"{"path": "new.txt"}"#, "n", "succeeded"),
                    view("write_file", r#"{"path": "new2.txt"}"#, "n", "succeeded"),
                ],
            ),
            (
                10,
                vec![view(
                    "read_file",
                    r#"{"path": "old.txt"}"#,
                    "o",
                    "succeeded",
                )],
            ),
        ]);
        assert_eq!(
            summary.lines,
            vec![
                "run 11: read_file(new.txt) -> succeeded (n)",
                "run 11: write_file(new2.txt) -> succeeded (n)",
                "run 10: read_file(old.txt) -> succeeded (o)",
            ]
        );
    }

    #[test]
    fn run_cap_keeps_three_newest_and_counts_omitted_runs() {
        let mut input: Vec<(i64, Vec<AgentStepView>)> = Vec::new();
        for run_id in [50, 40, 30, 20, 10] {
            input.push((
                run_id,
                vec![view("read_file", r#"{"path": "f.txt"}"#, "o", "succeeded")],
            ));
        }
        let summary = summarize(&input);
        assert_eq!(summary.lines.len(), MAX_PRIOR_RUNS);
        assert!(
            summary.lines[0].starts_with("run 50:"),
            "{}",
            summary.lines[0]
        );
        assert_eq!(summary.omitted_runs, 2);
        // The skipped runs' emittable steps are absent from context too.
        assert_eq!(summary.omitted_steps, 2);
    }

    #[test]
    fn line_cap_keeps_thirty_and_counts_omitted_steps() {
        let mut steps = Vec::new();
        for i in 0..35 {
            steps.push(view(
                "read_file",
                &format!(r#"{{"path": "f{i}.txt"}}"#),
                "o",
                "succeeded",
            ));
        }
        let summary = summarize(&[(1, steps)]);
        assert_eq!(summary.lines.len(), MAX_ACTION_LINES);
        assert_eq!(summary.omitted_runs, 0);
        assert_eq!(summary.omitted_steps, 5);
        let note = system_note(&summary).expect("non-empty summary has a note");
        assert!(
            note.ends_with("...(5 more steps omitted)"),
            "exact tail marker required, got: {note}"
        );
    }

    #[test]
    fn empty_input_yields_no_note() {
        let summary = summarize(&[]);
        assert!(summary.lines.is_empty());
        assert_eq!(summary.omitted_runs, 0);
        assert_eq!(summary.omitted_steps, 0);
        assert_eq!(system_note(&summary), None);
    }

    #[test]
    fn system_note_header_names_calls_and_runs() {
        let summary = summarize(&[
            (
                11,
                vec![view("read_file", r#"{"path": "n.txt"}"#, "n", "succeeded")],
            ),
            (
                10,
                vec![view("write_file", r#"{"path": "o.txt"}"#, "o", "succeeded")],
            ),
        ]);
        let note = system_note(&summary).expect("note");
        let mut lines = note.lines();
        assert_eq!(
            lines.next().expect("header"),
            "Prior action trace (2 calls across 2 prior run(s)):"
        );
        assert!(note.contains("run 11: read_file(n.txt) -> succeeded (n)"));
        // Nothing omitted: no tail marker.
        assert!(!note.contains("more steps omitted"), "{note}");
    }

    #[test]
    fn system_note_block_stays_near_6kb() {
        let mut steps = Vec::new();
        for i in 0..MAX_ACTION_LINES {
            steps.push(view(
                "execute_command",
                &format!(r#"{{"command": "run-{i:03}"}}"#),
                &"o".repeat(500),
                "succeeded",
            ));
        }
        let summary = summarize(&[(1, steps)]);
        let note = system_note(&summary).expect("note");
        assert!(
            note.len() <= 6656,
            "block must stay ~6KB, got {}",
            note.len()
        );
    }
}
