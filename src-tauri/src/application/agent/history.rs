//! Agent conversation-history window (agent memory slice).
//!
//! The agent persists its turns to the `messages` table but, until this
//! slice, never read them back: every run opened with only the system prompt
//! and the current request. This module bounds how much of that persisted
//! history enters one run. It owns the window logic so `runner.rs` stays a
//! thin consumer; see [`window`] and [`omitted_note`].

use crate::application::execution::{AiMessage, AiRole};

/// Default cap on the number of persisted history messages carried into one
/// agent run (soft bound: user-alignment may retain one extra message).
pub(crate) const DEFAULT_HISTORY_WINDOW: usize = 20;

/// A windowed view over persisted history: the retained tail plus how many
/// leading messages were left out.
pub(crate) struct Windowed {
    /// The retained history messages, in their original (oldest -> newest)
    /// order, always starting at a user message when non-empty.
    pub messages: Vec<AiMessage>,
    /// How many leading history messages were dropped.
    pub dropped: usize,
}

/// Keep at most `max` trailing messages of `history`, aligned so the window
/// starts at a user message.
///
/// - `max == 0` keeps nothing and reports the whole history as dropped.
/// - `history.len() <= max` keeps the whole slice with `dropped == 0`.
/// - Otherwise the last `max` messages are kept, then the start is walked
///   back while it points at a non-user message so the window never opens
///   mid-turn. `max` is therefore a soft bound: alignment may retain one
///   extra message (a persisted turn is at most `[user, assistant]`, but two
///   consecutive user messages are legal when a failed run persisted the
///   user turn and no assistant turn, and Anthropic requires the first
///   non-system message to be `user`).
pub(crate) fn window(history: &[AiMessage], max: usize) -> Windowed {
    if max == 0 {
        return Windowed {
            messages: Vec::new(),
            dropped: history.len(),
        };
    }
    if history.len() <= max {
        return Windowed {
            messages: history.to_vec(),
            dropped: 0,
        };
    }
    let mut start = history.len() - max;
    while start > 0 && history[start].role != AiRole::User {
        start -= 1;
    }
    Windowed {
        messages: history[start..].to_vec(),
        dropped: start,
    }
}

/// One-sentence system-prompt note stating that `dropped` earlier messages
/// of this conversation are not in context, directing the assistant to say
/// it lacks that part rather than guess.
pub(crate) fn omitted_note(dropped: usize) -> String {
    format!(
        "Note: {dropped} earlier messages of this conversation are not included in context; \
         if the user refers to them, say you no longer have that part of the history rather than guessing."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: &str) -> AiMessage {
        AiMessage {
            role: AiRole::User,
            content: content.to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }
    }

    fn assistant(content: &str) -> AiMessage {
        AiMessage {
            role: AiRole::Assistant,
            content: content.to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }
    }

    fn contents(windowed: &Windowed) -> Vec<&str> {
        windowed
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect()
    }

    #[test]
    fn window_empty_input_keeps_nothing() {
        let windowed = window(&[], DEFAULT_HISTORY_WINDOW);
        assert!(windowed.messages.is_empty());
        assert_eq!(windowed.dropped, 0);
    }

    #[test]
    fn window_shorter_than_max_is_unchanged() {
        let history = vec![user("q1"), assistant("a1"), user("q2")];
        let windowed = window(&history, DEFAULT_HISTORY_WINDOW);
        assert_eq!(contents(&windowed), vec!["q1", "a1", "q2"]);
        assert_eq!(windowed.dropped, 0);
    }

    #[test]
    fn window_longer_than_max_keeps_exact_tail() {
        let history = vec![
            user("q1"),
            assistant("a1"),
            user("q2"),
            assistant("a2"),
            user("q3"),
        ];
        let windowed = window(&history, 2);
        // The raw tail ["a2", "q3"] opens on an assistant turn, so the
        // alignment walk moves start back to "q2".
        assert_eq!(contents(&windowed), vec!["q2", "a2", "q3"]);
        assert_eq!(windowed.dropped, 2);
    }

    #[test]
    fn window_aligned_tail_needs_no_walk() {
        let history = vec![
            user("q1"),
            assistant("a1"),
            user("q2"),
            assistant("a2"),
            user("q3"),
            assistant("a3"),
        ];
        let windowed = window(&history, 2);
        // The raw tail already opens on a user message: kept exactly.
        assert_eq!(contents(&windowed), vec!["q3", "a3"]);
        assert_eq!(windowed.dropped, 4);
    }

    #[test]
    fn window_still_starts_at_a_user_message() {
        // History ending in a non-user run: the alignment walk must pull the
        // start back to the most recent user message.
        let history = vec![user("q1"), user("q2"), assistant("a2")];
        let windowed = window(&history, 1);
        assert!(!windowed.messages.is_empty());
        assert_eq!(windowed.messages[0].role, AiRole::User);
        assert_eq!(windowed.messages[0].content, "q2");
        assert_eq!(windowed.dropped, 1);
    }

    #[test]
    fn window_max_zero_drops_everything() {
        let history = vec![user("q1"), assistant("a1")];
        let windowed = window(&history, 0);
        assert!(windowed.messages.is_empty());
        assert_eq!(windowed.dropped, history.len());
    }

    #[test]
    fn omitted_note_states_count_and_no_guessing() {
        let note = omitted_note(7);
        assert!(note.contains('7'), "note must state the count: {note}");
        assert!(
            note.to_lowercase().contains("rather than guessing"),
            "note must direct the assistant not to guess: {note}"
        );
        assert!(
            note.len() < 300,
            "note must stay under ~300 characters: {note}"
        );
    }
}
