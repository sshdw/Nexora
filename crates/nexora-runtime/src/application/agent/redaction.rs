//! Streaming-safe secret redaction for command/terminal streams and persisted runs.
//!
//! Covers provider API keys (`sk-...`, `sk-ant-...`, `sk-proj-...`, `AIza...`,
//! `ghp_...`, `gho_...`, `ghu_...`, `ghs_...`, `ghr_...`, `github_pat_...`,
//! `xoxb-...`, `xoxp-...`, `xoxa-...`, `xoxo-...`, `xoxs-...`, `xoxr-...`,
//! `AKIA...`), `Bearer <token>` / `Basic <token>` auth schemes, URL
//! `user:pass@` credentials, `KEY`/`TOKEN`/`SECRET`/`PASSWORD` assignments, and
//! PEM private key blocks.

/// Canonical redaction marker for command and terminal stdout/stderr.
pub(crate) const COMMAND_REDACTED_MARKER: &str = "[REDACTED]";

/// Legacy lowercase redaction marker retained for existing persisted-step and
/// GitHub CI log consumers so historical assertions stay intact.
pub(crate) const LEGACY_REDACTED_MARKER: &str = "[redacted]";

/// Maximum partial line held back before flushing safe prefixes on
/// unbounded single-line output.
const MAX_PARTIAL_LINE_HOLD: usize = 4_096;

/// Maximum unclosed PEM block bytes held before fail-closed collapsing.
const MAX_PEM_BLOCK_HOLD: usize = 65_536;

/// Overlap retained when flushing an oversized un-newlined line.
const OVERSIZED_LINE_TAIL_KEEP: usize = 1_024;

/// Token prefixes scrubbed from output (`prefix` kept, secret value replaced).
/// Ordered longest/most-specific first so `sk-ant-` matches before `sk-`.
const DISTINCT_PREFIXES: &[&str] = &[
    "github_pat_",
    "sk-ant-",
    "sk-proj-",
    "AIza",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxo-",
    "xoxs-",
    "xoxr-",
    "AKIA",
];

const DISTINCT_PREFIX_MIN_VALUE: usize = 4;
const SK_PREFIX_MIN_VALUE: usize = 8;
const SHORT_PREFIX_MIN_VALUE: usize = 16;
const BEARER_TOKEN_MIN_VALUE: usize = 6;
const ASSIGNMENT_MIN_VALUE: usize = 6;

const ASSIGNMENT_FIELDS: &[&str] = &[
    "aws_secret_access_key",
    "api_key",
    "apikey",
    "passwd",
    "password",
    "secret",
    "token",
];

const NON_SECRET_KEY_PREFIXES: &[&str] = &[
    "primary_",
    "foreign_",
    "sort_",
    "cache_",
    "hash_",
    "row_",
    "partition_",
    "group_",
    "index_",
    "public_",
];

/// Convenience helper redacting command/terminal text with `[REDACTED]`.
#[must_use]
pub(crate) fn redact_command_output(text: &str) -> (String, bool) {
    redact_with_marker(text, COMMAND_REDACTED_MARKER)
}

/// Scrubs secrets from `text` using the supplied `marker` (for example
/// `[REDACTED]` or `[redacted]`), returning the scrubbed string and whether
/// at least one replacement occurred.
#[must_use]
pub(crate) fn redact_with_marker(text: &str, marker: &str) -> (String, bool) {
    redact(text, marker, false)
}

/// Redact a bounded prompt source while preserving its original line offsets.
pub(crate) fn redact_prompt_source(text: &str) -> String {
    redact(text, LEGACY_REDACTED_MARKER, true).0
}

fn redact(text: &str, marker: &str, preserve_pem_lines: bool) -> (String, bool) {
    let (blocked, pem_hit) = redact_pem_blocks(text, marker, preserve_pem_lines);
    let (url_scrubbed, url_hit) = redact_url_userinfo(&blocked, marker);
    let (prefix_scrubbed, prefix_hit) = redact_schemes_and_prefixes(&url_scrubbed, marker);
    let (assignment_scrubbed, assignment_hit) = redact_assignment_values(&prefix_scrubbed, marker);
    (
        assignment_scrubbed,
        pem_hit || url_hit || prefix_hit || assignment_hit,
    )
}

/// Stateful, UTF-8 safe streaming secret redactor that holds back partial lines
/// or open secret candidates across pipe read boundaries so secrets split across
/// chunks are never emitted unredacted.
pub(crate) struct StreamRedactor {
    marker: &'static str,
    utf8_tail: [u8; 3],
    utf8_tail_len: usize,
    pending: String,
    in_pem_block: bool,
    any_redacted: bool,
}

impl StreamRedactor {
    #[must_use]
    pub(crate) fn for_command() -> Self {
        Self::new(COMMAND_REDACTED_MARKER)
    }

    #[must_use]
    pub(crate) fn new(marker: &'static str) -> Self {
        Self {
            marker,
            utf8_tail: [0; 3],
            utf8_tail_len: 0,
            pending: String::new(),
            in_pem_block: false,
            any_redacted: false,
        }
    }

    #[must_use]
    pub(crate) fn any_redacted(&self) -> bool {
        self.any_redacted
    }

    /// Feeds raw bytes from stdout/stderr, buffers incomplete UTF-8 codepoints
    /// and trailing partial tokens, and returns the safely redacted text ready
    /// to emit right now.
    pub(crate) fn push_bytes(&mut self, bytes: &[u8]) -> String {
        self.decode_utf8_into_pending(bytes);
        let mut emitted = String::new();

        loop {
            if self.in_pem_block {
                if let Some(end_pos) = find_ascii_ci(self.pending.as_bytes(), b"-----end", 0) {
                    if let Some(nl_rel) = self.pending[end_pos..].find('\n') {
                        self.pending.drain(..end_pos + nl_rel);
                        self.in_pem_block = false;
                        continue;
                    }
                    if end_pos > 0 {
                        self.pending.drain(..end_pos);
                    }
                    if self.pending.len() >= MAX_PARTIAL_LINE_HOLD {
                        if let Some(dashes) = find_ascii_ci(self.pending.as_bytes(), b"-----", 8) {
                            self.pending.drain(..dashes + 5);
                            self.in_pem_block = false;
                            continue;
                        }
                    }
                    break;
                }
                if self.pending.len() > 32 {
                    let keep_from = floor_char_boundary(&self.pending, self.pending.len() - 32);
                    self.pending.drain(..keep_from);
                }
                break;
            }

            if self.pending.len() >= MAX_PEM_BLOCK_HOLD
                && find_ascii_ci(self.pending.as_bytes(), b"-----begin", 0) == Some(0)
                && !has_closed_pem_at_start(&self.pending)
            {
                emitted.push_str("-----BEGIN ");
                emitted.push_str(self.marker);
                self.any_redacted = true;
                self.in_pem_block = true;
                let keep_from = floor_char_boundary(&self.pending, self.pending.len() - 32).max(10);
                self.pending.drain(..keep_from);
                continue;
            }

            let flush_end = safe_flush_end(&self.pending);
            if flush_end == 0 {
                if self.pending.len() >= MAX_PARTIAL_LINE_HOLD
                    && find_ascii_ci(self.pending.as_bytes(), b"-----begin", 0).is_none()
                {
                    compact_oversized_open_secret(&mut self.pending);
                }
                break;
            }

            let prefix: String = self.pending.drain(..flush_end).collect();
            let (scrubbed, hit) = redact_with_marker(&prefix, self.marker);
            self.any_redacted |= hit;
            emitted.push_str(&scrubbed);
        }

        emitted
    }

    /// Flushes any buffered trailing bytes/lines at EOF and returns the final
    /// redacted chunk paired with whether any secret was redacted across the
    /// whole stream.
    pub(crate) fn finish(&mut self) -> (String, bool) {
        if self.utf8_tail_len > 0 {
            self.pending.push('\u{FFFD}');
            self.utf8_tail_len = 0;
        }
        if self.in_pem_block {
            self.in_pem_block = false;
            if let Some(end_pos) = find_ascii_ci(self.pending.as_bytes(), b"-----end", 0) {
                if let Some(nl_rel) = self.pending[end_pos..].find('\n') {
                    self.pending.drain(..end_pos + nl_rel);
                } else if let Some(dashes) =
                    find_ascii_ci(self.pending.as_bytes(), b"-----", end_pos + 8)
                {
                    self.pending.drain(..dashes + 5);
                } else {
                    self.pending.clear();
                }
            } else {
                self.pending.clear();
            }
        }
        let rest = std::mem::take(&mut self.pending);
        if rest.is_empty() {
            return (String::new(), self.any_redacted);
        }
        let (scrubbed, hit) = redact_with_marker(&rest, self.marker);
        self.any_redacted |= hit;
        (scrubbed, self.any_redacted)
    }

    fn decode_utf8_into_pending(&mut self, mut input: &[u8]) {
        if self.utf8_tail_len > 0 && !input.is_empty() {
            let mut combined = [0_u8; 4];
            let tail_len = self.utf8_tail_len;
            combined[..tail_len].copy_from_slice(&self.utf8_tail[..tail_len]);
            let take = input.len().min(4 - tail_len);
            combined[tail_len..tail_len + take].copy_from_slice(&input[..take]);
            let slice = &combined[..tail_len + take];

            match std::str::from_utf8(slice) {
                Ok(valid) => {
                    self.pending.push_str(valid);
                    self.utf8_tail_len = 0;
                    input = &input[take..];
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        if let Ok(valid) = std::str::from_utf8(&slice[..valid_up_to]) {
                            self.pending.push_str(valid);
                        }
                    }
                    if let Some(bad_len) = err.error_len() {
                        self.pending.push('\u{FFFD}');
                        let after = valid_up_to + bad_len;
                        self.utf8_tail_len = 0;
                        if after < tail_len {
                            // Remaining bytes belonged to `utf8_tail`, consume zero new bytes
                            // except those already not needed, so preserve trailing tail bytes.
                            let rem = tail_len - after;
                            self.utf8_tail[..rem].copy_from_slice(&combined[after..tail_len]);
                            self.utf8_tail_len = rem;
                            self.decode_utf8_into_pending(input);
                            return;
                        }
                        let consumed_from_input = after.saturating_sub(tail_len);
                        input = &input[consumed_from_input..];
                    } else {
                        let rem = slice.len() - valid_up_to;
                        self.utf8_tail[..rem].copy_from_slice(&slice[valid_up_to..]);
                        self.utf8_tail_len = rem;
                        return;
                    }
                }
            }
        }

        let mut cursor = 0;
        while cursor < input.len() {
            match std::str::from_utf8(&input[cursor..]) {
                Ok(valid) => {
                    self.pending.push_str(valid);
                    break;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        if let Ok(valid) = std::str::from_utf8(&input[cursor..cursor + valid_up_to])
                        {
                            self.pending.push_str(valid);
                        }
                    }
                    cursor += valid_up_to;
                    if let Some(bad_len) = err.error_len() {
                        self.pending.push('\u{FFFD}');
                        cursor += bad_len;
                    } else {
                        let rem = input.len() - cursor;
                        let copy_len = rem.min(self.utf8_tail.len());
                        self.utf8_tail[..copy_len]
                            .copy_from_slice(&input[cursor..cursor + copy_len]);
                        self.utf8_tail_len = copy_len;
                        break;
                    }
                }
            }
        }
    }
}

fn find_ascii_ci(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|pos| from + pos)
}

fn has_closed_pem_at_start(pending: &str) -> bool {
    let bytes = pending.as_bytes();
    if let Some(end_pos) = find_ascii_ci(bytes, b"-----end", 10) {
        return pending[end_pos..].contains('\n');
    }
    false
}

fn safe_flush_end(pending: &str) -> usize {
    if pending.is_empty() {
        return 0;
    }

    // Hold back any open PEM block until `-----END ...-----\n` arrives or the
    // fail-closed hold cap trips (handled via `in_pem_block` in `StreamRedactor`).
    let bytes = pending.as_bytes();
    let mut scan = 0;
    while let Some(begin) = find_ascii_ci(bytes, b"-----begin", scan) {
        if let Some(end_pos) = find_ascii_ci(bytes, b"-----end", begin + 10) {
            if let Some(nl_rel) = pending[end_pos..].find('\n') {
                scan = end_pos + nl_rel + 1;
                continue;
            }
        }
        if begin > 0 {
            if pending.len() - begin >= MAX_PEM_BLOCK_HOLD {
                return begin;
            }
            return safe_flush_end(&pending[..begin]);
        }
        return 0;
    }

    let baseline = pending.rfind('\n').map_or(0, |nl| nl + 1);
    let tail = &pending[baseline..];
    if tail.is_empty() {
        return baseline;
    }

    if tail.len() >= MAX_PARTIAL_LINE_HOLD {
        let safe_in_tail = safe_flush_in_oversized_tail(tail);
        return baseline + safe_in_tail;
    }

    if contains_secret_starter(tail) {
        return baseline;
    }

    // No `\n` yet and no open secret starter in the tail: keep the last two
    // whitespace tokens buffered so split prefixes like `Bear` + `er tok` or
    // `MY_` + `KEY=val` never leak before completion, while flushing earlier
    // progress words immediately.
    let mut spaces = tail
        .bytes()
        .enumerate()
        .filter_map(|(idx, b)| matches!(b, b' ' | b'\t').then_some(idx));
    if let (Some(_last), Some(second_last)) = (spaces.next_back(), spaces.next_back()) {
        return baseline + second_last + 1;
    }

    baseline
}

fn safe_flush_in_oversized_tail(tail: &str) -> usize {
    let target = tail.len().saturating_sub(OVERSIZED_LINE_TAIL_KEEP);
    let spans = find_secret_candidate_spans(tail);
    let mut cut = tail[..target]
        .rfind([' ', '\t'])
        .map_or_else(|| floor_char_boundary(tail, target), |ws| ws + 1);

    loop {
        let mut adjusted = false;
        for &(span_start, span_end) in &spans {
            if span_start < cut && cut < span_end {
                cut = span_start;
                adjusted = true;
            }
        }
        if !adjusted {
            break;
        }
    }

    if cut > 0 {
        return cut;
    }

    // A secret candidate starting at index 0 crossed `target`. If that candidate
    // (and any overlapping span) has already terminated before `tail.len()`, flush
    // through its end so it is redacted as a complete unit right now; otherwise
    // return 0 so `StreamRedactor::push_bytes` compacts the open secret in place.
    let mut end_from_zero = 0;
    for &(span_start, span_end) in &spans {
        if span_start == 0 && span_end > end_from_zero {
            end_from_zero = span_end;
        }
    }
    if end_from_zero > 0 {
        loop {
            let mut extended = false;
            for &(span_start, span_end) in &spans {
                if span_start < end_from_zero && end_from_zero < span_end {
                    end_from_zero = span_end;
                    extended = true;
                }
            }
            if !extended {
                break;
            }
        }
        if end_from_zero < tail.len() {
            return end_from_zero;
        }
    }

    0
}

fn find_secret_candidate_spans(tail: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    collect_url_spans(tail, &mut spans);
    collect_scheme_and_prefix_spans(tail, &mut spans);
    collect_assignment_spans(tail, &mut spans);
    spans
}

fn collect_url_spans(tail: &str, spans: &mut Vec<(usize, usize)>) {
    let bytes = tail.as_bytes();
    let mut cursor = 0;
    while let Some(rel) = tail[cursor..].find("://") {
        let sep = cursor + rel;
        let mut scheme_start = sep;
        while scheme_start > 0 && is_scheme_char(bytes[scheme_start - 1]) {
            scheme_start -= 1;
        }
        let mut auth_end = sep + 3;
        while auth_end < bytes.len()
            && !matches!(
                bytes[auth_end],
                b'/' | b'?' | b'#' | b'"' | b'\'' | b'>' | b')' | b' ' | b'\t' | b'\r' | b'\n'
            )
        {
            auth_end += 1;
        }
        spans.push((floor_char_boundary(tail, scheme_start), auth_end));
        cursor = auth_end.max(sep + 3);
    }
}

fn collect_scheme_and_prefix_spans(tail: &str, spans: &mut Vec<(usize, usize)>) {
    let bytes = tail.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let scheme_len = if index + 7 <= bytes.len()
            && bytes[index..index + 7].eq_ignore_ascii_case(b"bearer ")
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
        {
            Some(7)
        } else if index + 6 <= bytes.len()
            && (bytes[index..index + 6].eq_ignore_ascii_case(b"basic ")
                || bytes[index..index + 6].eq_ignore_ascii_case(b"token "))
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
        {
            Some(6)
        } else {
            None
        };

        if let Some(len) = scheme_len {
            let trimmed_before = tail[..index].trim_end_matches([' ', '\t']);
            let span_start = if trimmed_before
                .to_ascii_lowercase()
                .ends_with("authorization:")
            {
                floor_char_boundary(tail, trimmed_before.len().saturating_sub(14))
            } else {
                index
            };
            let mut end = index + len;
            while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
                end += 1;
            }
            while end < bytes.len() && is_token_byte(bytes[end]) {
                end += 1;
            }
            spans.push((span_start, end));
            index = end.max(index + len);
            continue;
        }

        if let Some((prefix_len, _)) = match_secret_prefix(bytes, index) {
            let span_start = if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
                floor_char_boundary(tail, index - 1)
            } else {
                index
            };
            let mut end = index + prefix_len;
            while end < bytes.len() && is_token_byte(bytes[end]) {
                end += 1;
            }
            spans.push((span_start, end));
            index = end.max(index + prefix_len);
            continue;
        }

        index += 1;
    }
}

fn collect_assignment_spans(tail: &str, spans: &mut Vec<(usize, usize)>) {
    let lowered = tail.to_ascii_lowercase();
    let bytes = tail.as_bytes();
    let low_bytes = lowered.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        let (field_len, require_equals, _) = match_assignment_field(low_bytes, index);
        if field_len == 0 {
            index += 1;
            continue;
        }
        let mut span_start = index;
        while span_start > 0
            && index - span_start < 32
            && (bytes[span_start - 1].is_ascii_alphanumeric()
                || matches!(bytes[span_start - 1], b'_' | b'-'))
        {
            span_start -= 1;
        }
        span_start = floor_char_boundary(tail, span_start);

        let Some(value_start) =
            find_assignment_value_start(bytes, index, field_len, require_equals)
        else {
            index += field_len;
            continue;
        };

        let span_end = if value_start >= bytes.len() {
            bytes.len()
        } else if matches!(bytes[value_start], b'"' | b'\'') {
            let quote = bytes[value_start];
            let mut end = value_start + 1;
            while end < bytes.len() && bytes[end] != quote && !matches!(bytes[end], b'\r' | b'\n') {
                end += 1;
            }
            if end < bytes.len() && bytes[end] == quote {
                end += 1;
            }
            end
        } else {
            let mut end = value_start;
            while end < bytes.len()
                && !matches!(
                    bytes[end],
                    b' ' | b'\t' | b'\r' | b'\n' | b'&' | b'"' | b'\''
                )
            {
                end += 1;
            }
            end
        };

        spans.push((span_start, span_end));
        index = span_end.max(index + field_len);
    }
}

fn compact_oversized_open_secret(pending: &mut String) {
    const KEEP_HEAD: usize = 256;
    const KEEP_TAIL: usize = 256;
    if pending.len() <= KEEP_HEAD + KEEP_TAIL {
        return;
    }
    let mut head_target = KEEP_HEAD;
    if let Some(sep) = pending.find("://") {
        if let Some(colon_rel) = pending[sep + 3..].find(':') {
            let pass_start = sep + 3 + colon_rel + 1;
            head_target = head_target.max(pass_start.saturating_add(64));
        } else {
            // URL without `:` in authority has no user:pass password to compact;
            // drain interior authority bytes safely after `sep + 3 + 64`.
            head_target = head_target.max(sep + 3 + 64);
        }
    }
    let tail_start = floor_char_boundary(pending, pending.len().saturating_sub(KEEP_TAIL));
    let head_end = floor_char_boundary(pending, head_target.min(tail_start));
    if head_end < tail_start {
        pending.drain(head_end..tail_start);
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn contains_secret_starter(tail: &str) -> bool {
    const STARTERS: &[&str] = &[
        "bearer",
        "basic",
        "sk-",
        "aiza",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "xox",
        "akia",
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "key",
    ];
    let bytes = tail.as_bytes();
    if bytes.iter().any(|b| matches!(b, b'=' | b':' | b'@' | b'-')) {
        return true;
    }
    let lowered = tail.to_ascii_lowercase();
    STARTERS.iter().any(|needle| lowered.contains(needle))
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'~' | b'+' | b'/' | b'.' | b'=')
}

fn is_scheme_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-')
}

fn redact_url_userinfo(text: &str, marker: &str) -> (String, bool) {
    let bytes = text.as_bytes();
    let mut scrubbed = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut hit = false;

    while let Some(rel) = text[cursor..].find("://") {
        let sep = cursor + rel;
        let mut scheme_start = sep;
        while scheme_start > cursor && is_scheme_char(bytes[scheme_start - 1]) {
            scheme_start -= 1;
        }
        let scheme_valid = scheme_start < sep
            && bytes[scheme_start].is_ascii_alphabetic()
            && (scheme_start == 0
                || !(bytes[scheme_start - 1].is_ascii_alphanumeric()
                    || bytes[scheme_start - 1] == b'_'));
        if !scheme_valid {
            scrubbed.push_str(&text[cursor..sep + 3]);
            cursor = sep + 3;
            continue;
        }

        let auth_start = sep + 3;
        let mut auth_end = auth_start;
        while auth_end < bytes.len()
            && !matches!(
                bytes[auth_end],
                b'/' | b'?' | b'#' | b'"' | b'\'' | b'>' | b')' | b' ' | b'\t' | b'\r' | b'\n'
            )
        {
            auth_end += 1;
        }

        let authority = &text[auth_start..auth_end];
        if let Some(at_rel) = authority.rfind('@') {
            let userinfo = &authority[..at_rel];
            if let Some(colon_rel) = userinfo.find(':') {
                let pass_start = auth_start + colon_rel + 1;
                let at_abs = auth_start + at_rel;
                if pass_start < at_abs && &text[pass_start..at_abs] != marker {
                    scrubbed.push_str(&text[cursor..pass_start]);
                    scrubbed.push_str(marker);
                    hit = true;
                    cursor = at_abs;
                    continue;
                }
            }
        }

        scrubbed.push_str(&text[cursor..auth_end]);
        cursor = auth_end;
    }

    scrubbed.push_str(&text[cursor..]);
    (scrubbed, hit)
}

fn redact_schemes_and_prefixes(text: &str, marker: &str) -> (String, bool) {
    let bytes = text.as_bytes();
    let mut scrubbed = String::with_capacity(text.len());
    let mut index = 0;
    let mut hit = false;

    while index < bytes.len() {
        if index + 7 <= bytes.len()
            && bytes[index..index + 7].eq_ignore_ascii_case(b"bearer ")
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
        {
            let before = &text[..index];
            let after_auth_header = before
                .trim_end_matches([' ', '\t'])
                .to_ascii_lowercase()
                .ends_with("authorization:");
            scrubbed.push_str(&text[index..index + 7]);
            index += 7;
            while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
                scrubbed.push(bytes[index] as char);
                index += 1;
            }
            let value_start = index;
            while index < bytes.len() && is_token_byte(bytes[index]) {
                index += 1;
            }
            let value = &text[value_start..index];
            let should_redact = value != marker
                && (value.len() >= SHORT_PREFIX_MIN_VALUE
                    || (value.len() >= BEARER_TOKEN_MIN_VALUE
                        && (after_auth_header || value.bytes().any(|b| !b.is_ascii_lowercase()))));
            if should_redact {
                scrubbed.push_str(marker);
                hit = true;
            } else {
                scrubbed.push_str(value);
            }
            continue;
        }

        if index + 6 <= bytes.len()
            && bytes[index..index + 6].eq_ignore_ascii_case(b"basic ")
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
        {
            scrubbed.push_str(&text[index..index + 6]);
            index += 6;
            let value_start = index;
            while index < bytes.len() && is_token_byte(bytes[index]) {
                index += 1;
            }
            let value = &text[value_start..index];
            if value != marker && value.len() >= SHORT_PREFIX_MIN_VALUE {
                scrubbed.push_str(marker);
                hit = true;
            } else {
                scrubbed.push_str(value);
            }
            continue;
        }

        if let Some((prefix_len, min_len)) = match_secret_prefix(bytes, index) {
            scrubbed.push_str(&text[index..index + prefix_len]);
            index += prefix_len;
            let value_start = index;
            while index < bytes.len() && is_token_byte(bytes[index]) {
                index += 1;
            }
            let value = &text[value_start..index];
            if value != marker && value.len() >= min_len {
                scrubbed.push_str(marker);
                hit = true;
            } else {
                scrubbed.push_str(value);
            }
            continue;
        }

        match text[index..].chars().next() {
            Some(ch) => {
                scrubbed.push(ch);
                index += ch.len_utf8();
            }
            None => break,
        }
    }

    (scrubbed, hit)
}

fn match_secret_prefix(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    for prefix in DISTINCT_PREFIXES {
        if index + prefix.len() <= bytes.len()
            && bytes[index..index + prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        {
            return Some((prefix.len(), DISTINCT_PREFIX_MIN_VALUE));
        }
    }

    if index + 3 <= bytes.len() && bytes[index..index + 3].eq_ignore_ascii_case(b"sk-") {
        let preceded_by_alnum = index > 0 && bytes[index - 1].is_ascii_alphanumeric();
        let min_len = if preceded_by_alnum {
            SHORT_PREFIX_MIN_VALUE
        } else {
            SK_PREFIX_MIN_VALUE
        };
        return Some((3, min_len));
    }

    None
}

fn is_assignment_gap(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}

fn find_assignment_value_start(
    bytes: &[u8],
    index: usize,
    field_len: usize,
    require_equals: bool,
) -> Option<usize> {
    let mut cursor = index + field_len;
    while cursor < bytes.len()
        && (bytes[cursor].is_ascii_alphanumeric() || matches!(bytes[cursor], b'_' | b'-'))
    {
        cursor += 1;
    }
    while cursor < bytes.len() && is_assignment_gap(bytes[cursor]) {
        cursor += 1;
    }
    if cursor < bytes.len() && matches!(bytes[cursor], b'"' | b'\'') {
        cursor += 1;
        while cursor < bytes.len() && is_assignment_gap(bytes[cursor]) {
            cursor += 1;
        }
    }
    let sep_ok = if require_equals {
        cursor < bytes.len() && bytes[cursor] == b'='
    } else {
        cursor < bytes.len() && matches!(bytes[cursor], b':' | b'=')
    };
    if !sep_ok {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len() && is_assignment_gap(bytes[cursor]) {
        cursor += 1;
    }
    Some(cursor)
}

fn redact_assignment_value_at(
    text: &str,
    index: usize,
    value_start: usize,
    min_len: usize,
    marker: &str,
    scrubbed: &mut String,
) -> (usize, bool) {
    let bytes = text.as_bytes();
    if value_start < bytes.len() && matches!(bytes[value_start], b'"' | b'\'') {
        let quote = bytes[value_start];
        let inner_start = value_start + 1;
        let mut inner_end = inner_start;
        while inner_end < bytes.len()
            && bytes[inner_end] != quote
            && !matches!(bytes[inner_end], b'\r' | b'\n')
        {
            inner_end += 1;
        }
        let inner = &text[inner_start..inner_end];
        let closed = inner_end < bytes.len() && bytes[inner_end] == quote;
        let next_index = if closed { inner_end + 1 } else { inner_end };
        if inner != marker && inner.len() >= min_len {
            scrubbed.push_str(&text[index..inner_start]);
            scrubbed.push_str(marker);
            if closed {
                scrubbed.push(quote as char);
            }
            return (next_index, true);
        }
        scrubbed.push_str(&text[index..next_index]);
        return (next_index, false);
    }

    let mut cursor = value_start;
    while cursor < bytes.len()
        && !matches!(
            bytes[cursor],
            b' ' | b'\t' | b'\r' | b'\n' | b'&' | b'"' | b'\''
        )
    {
        cursor += 1;
    }
    let value = &text[value_start..cursor];
    if value != marker && value.len() >= min_len {
        scrubbed.push_str(&text[index..value_start]);
        scrubbed.push_str(marker);
        return (cursor, true);
    }
    scrubbed.push_str(&text[index..cursor]);
    (cursor, false)
}

fn redact_assignment_values(text: &str, marker: &str) -> (String, bool) {
    let lowered = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let low_bytes = lowered.as_bytes();
    let mut scrubbed = String::with_capacity(text.len());
    let mut index = 0;
    let mut hit = false;

    while index < bytes.len() {
        let (field_len, require_equals, min_len) = match_assignment_field(low_bytes, index);
        if field_len == 0 {
            match text[index..].chars().next() {
                Some(ch) => {
                    scrubbed.push(ch);
                    index += ch.len_utf8();
                }
                None => break,
            }
            continue;
        }

        let Some(value_start) =
            find_assignment_value_start(bytes, index, field_len, require_equals)
        else {
            scrubbed.push_str(&text[index..index + field_len]);
            index += field_len;
            continue;
        };

        let (next_index, redacted) =
            redact_assignment_value_at(text, index, value_start, min_len, marker, &mut scrubbed);
        hit |= redacted;
        index = next_index;
    }

    (scrubbed, hit)
}

fn match_assignment_field(low_bytes: &[u8], index: usize) -> (usize, bool, usize) {
    let mut best_len = 0;
    for field in ASSIGNMENT_FIELDS {
        if field.len() > best_len
            && index + field.len() <= low_bytes.len()
            && low_bytes[index..index + field.len()] == *field.as_bytes()
        {
            best_len = field.len();
        }
    }
    if best_len > 0 {
        return (best_len, false, ASSIGNMENT_MIN_VALUE);
    }

    // Match `KEY=...` and `*_KEY=...` assignments while excluding prose (`key: value`)
    // and common non-secret key identifiers (`primary_key=`, `sort_key=`, etc.).
    if index + 3 <= low_bytes.len() && &low_bytes[index..index + 3] == b"key" {
        let valid_left = index == 0 || !low_bytes[index - 1].is_ascii_alphanumeric();
        let before = &low_bytes[..index];
        let excluded = NON_SECRET_KEY_PREFIXES
            .iter()
            .any(|prefix| before.ends_with(prefix.as_bytes()));
        if valid_left && !excluded {
            return (3, true, ASSIGNMENT_MIN_VALUE);
        }
    }

    (0, false, ASSIGNMENT_MIN_VALUE)
}

fn redact_pem_blocks(text: &str, marker: &str, preserve_lines: bool) -> (String, bool) {
    let bytes = text.as_bytes();
    let mut scrubbed = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut hit = false;

    while let Some(begin) = find_ascii_ci(bytes, b"-----begin", cursor) {
        let end = find_ascii_ci(bytes, b"-----end", begin + 9).map_or(text.len(), |end| {
            text[end..]
                .find('\n')
                .map_or(text.len(), |offset| end + offset)
        });
        scrubbed.push_str(&text[cursor..begin]);
        if preserve_lines {
            // Avoid leaving an open PEM delimiter in the sanitized source.
            scrubbed.push_str(marker);
            scrubbed.extend(text[begin..end].chars().filter(|&ch| ch == '\n'));
        } else {
            scrubbed.push_str("-----BEGIN ");
            scrubbed.push_str(marker);
        }
        hit = true;
        cursor = end;
    }

    scrubbed.push_str(&text[cursor..]);
    (scrubbed, hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_redactor_scrubs_all_required_secret_classes_with_uppercase_marker() {
        let input = "\
openai=sk-abcdefghijklmnop1234
anthropic=sk-ant-api03-secretvalue9876
google=AIzaSyDummyGoogleApiKey12345
gh1=ghp_0123456789abcdef0123
gh2=gho_0123456789abcdef0123
gh3=github_pat_0123456789abcdef
slack=xoxb-1234567890-secret
aws=AKIAIOSFODNN7EXAMPLE
Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig
git clone https://alice:super_pass_99@github.com/org/repo.git
OPENAI_KEY=supersecretvalue99
AUTH_TOKEN=\"tok_secret_value_123\"
CLIENT_SECRET=mysecretvalue123
DB_PASSWORD=hunter2password
";
        let (scrubbed, hit) = redact_command_output(input);
        assert!(hit);
        assert!(scrubbed.contains("[REDACTED]"));
        for secret in [
            "abcdefghijklmnop1234",
            "api03-secretvalue9876",
            "SyDummyGoogleApiKey12345",
            "0123456789abcdef0123",
            "0123456789abcdef",
            "1234567890-secret",
            "IOSFODNN7EXAMPLE",
            "eyJhbGciOiJIUzI1NiJ9.payload.sig",
            "super_pass_99",
            "supersecretvalue99",
            "tok_secret_value_123",
            "mysecretvalue123",
            "hunter2password",
        ] {
            assert!(
                !scrubbed.contains(secret),
                "secret {secret:?} leaked into scrubbed output:\n{scrubbed}"
            );
        }
        assert!(scrubbed.contains("https://alice:[REDACTED]@github.com/org/repo.git"));
        assert!(scrubbed.contains("Authorization: Bearer [REDACTED]"));
        assert!(scrubbed.contains("OPENAI_KEY=[REDACTED]"));
        assert!(scrubbed.contains("AUTH_TOKEN=\"[REDACTED]\""));
    }

    #[test]
    fn stream_redactor_scrubs_secrets_split_across_arbitrary_byte_boundaries() {
        let full = concat!(
            "step1 ok \u{1F680}\n",
            "url=https://deploy:very_secret_pass_42@registry.example.com/app\n",
            "auth: Bearer super_secret_token_xyz_123\n",
            "key: AIzaSySplitAcrossChunks999\n",
            "API_KEY=split_assignment_secret_val\n",
            "tail done"
        );

        // Feed 3 bytes at a time to slice right through multi-byte UTF-8 AND
        // every secret prefix/URL/assignment boundary.
        let mut redactor = StreamRedactor::for_command();
        let mut collected = String::new();
        for chunk in full.as_bytes().chunks(3) {
            let emitted = redactor.push_bytes(chunk);
            // At no point may any partial emitted chunk contain raw secret fragments.
            for fragment in [
                "very_secret_pass_42",
                "super_secret_token_xyz_123",
                "SySplitAcrossChunks999",
                "split_assignment_secret_val",
            ] {
                assert!(
                    !emitted.contains(fragment),
                    "emitted intermediate chunk leaked {fragment:?}: {emitted:?}"
                );
            }
            collected.push_str(&emitted);
        }
        let (tail, hit) = redactor.finish();
        collected.push_str(&tail);

        assert!(hit);
        assert!(collected.contains("step1 ok \u{1F680}"));
        assert!(collected.contains("https://deploy:[REDACTED]@registry.example.com/app"));
        assert!(collected.contains("Bearer [REDACTED]"));
        assert!(collected.contains("AIza[REDACTED]"));
        assert!(collected.contains("API_KEY=[REDACTED]"));
        assert!(collected.contains("tail done"));
    }

    #[test]
    fn stream_redactor_preserves_benign_prose_and_non_secret_keys() {
        let prose = "primary_key=user_id\nsort_key=created_at\nkey insights for the quarter\nhttps://github.com/org/repo.git\nrisk-free task-setup\nBearer of good news\npassword: wrong\n";
        let mut redactor = StreamRedactor::for_command();
        let mut out = redactor.push_bytes(prose.as_bytes());
        let (tail, hit) = redactor.finish();
        out.push_str(&tail);

        assert!(!hit, "benign prose should not trigger redaction: {out:?}");
        assert_eq!(out, prose);
    }

    #[test]
    fn stream_redactor_scrubs_bearer_and_sk_split_at_max_partial_line_hold() {
        // Case 1: "Bearer " lands right before `MAX_PARTIAL_LINE_HOLD - OVERSIZED_LINE_TAIL_KEEP` (3072)
        // on a single un-newlined line, and its token spans across 3072 and 4096 into the next chunk.
        let prefix_words = "a ".repeat(1532); // 3064 bytes
        let secret_part1 = "T".repeat(1025); // 3064 + 7 ("Bearer ") + 1025 = 4096 bytes
        let secret_part2 = "U".repeat(64);
        let chunk1 = format!("{prefix_words}Bearer {secret_part1}");
        assert_eq!(chunk1.len(), MAX_PARTIAL_LINE_HOLD);
        let chunk2 = format!("{secret_part2} done\n");

        let mut redactor = StreamRedactor::for_command();
        let out1 = redactor.push_bytes(chunk1.as_bytes());
        let out2 = redactor.push_bytes(chunk2.as_bytes());
        let (out3, hit) = redactor.finish();
        let combined = format!("{out1}{out2}{out3}");

        assert!(hit);
        assert!(
            !combined.contains("TTTT") && !combined.contains("UUUU"),
            "Bearer token split at MAX_PARTIAL_LINE_HOLD leaked: {combined:?}"
        );
        assert!(combined.contains("Bearer [REDACTED] done\n"));

        // Case 2: Unspaced line where `sk-...` straddles the target cut boundary (3072)
        // and continues across MAX_PARTIAL_LINE_HOLD (4096) into the next chunk.
        let unspaced_prefix = "x".repeat(3070);
        let sk_part1 = format!("sk-{}", "S".repeat(1023)); // 3070 + 1026 = 4096 bytes
        let chunk1_sk = format!("{unspaced_prefix}{sk_part1}");
        assert_eq!(chunk1_sk.len(), MAX_PARTIAL_LINE_HOLD);
        let chunk2_sk = format!("{}\n", "Z".repeat(48));

        let mut redactor_sk = StreamRedactor::for_command();
        let sk_out1 = redactor_sk.push_bytes(chunk1_sk.as_bytes());
        let sk_out2 = redactor_sk.push_bytes(chunk2_sk.as_bytes());
        let (sk_out3, sk_hit) = redactor_sk.finish();
        let sk_combined = format!("{sk_out1}{sk_out2}{sk_out3}");

        assert!(sk_hit);
        assert!(
            !sk_combined.contains("SSSS") && !sk_combined.contains("ZZZZ"),
            "sk- token split at MAX_PARTIAL_LINE_HOLD leaked: {sk_combined:?}"
        );
        assert!(sk_combined.contains("sk-[REDACTED]\n"));

        // Case 3: Single oversized `sk-...` starting at byte 0 that exceeds MAX_PARTIAL_LINE_HOLD.
        let mut redactor_huge = StreamRedactor::for_command();
        let huge_chunk1 = format!("sk-{}", "K".repeat(MAX_PARTIAL_LINE_HOLD + 500));
        let huge_chunk2 = format!("{} tail\n", "M".repeat(200));
        let h1 = redactor_huge.push_bytes(huge_chunk1.as_bytes());
        let h2 = redactor_huge.push_bytes(huge_chunk2.as_bytes());
        let (h3, huge_hit) = redactor_huge.finish();
        let huge_combined = format!("{h1}{h2}{h3}");

        assert!(huge_hit);
        assert!(
            !huge_combined.contains("KKKK") && !huge_combined.contains("MMMM"),
            "oversized sk- token leaked: {huge_combined:?}"
        );
        assert_eq!(huge_combined, "sk-[REDACTED] tail\n");
    }

    #[test]
    fn stream_redactor_tracks_in_pem_block_across_max_pem_block_hold() {
        let header = "before-pem\n-----BEGIN RSA PRIVATE KEY-----\n";
        let body_line = "MIIEpAIBAAKCAQEA0123456789abcdef0123456789abcdef0123456789abcdef\n";
        let repeats = (MAX_PEM_BLOCK_HOLD / body_line.len()) + 32;
        let oversized_body = body_line.repeat(repeats);
        assert!(oversized_body.len() > MAX_PEM_BLOCK_HOLD);
        let footer = "SECRET_PEM_TAIL_LINE_999\n-----END RSA PRIVATE KEY-----\nafter-pem\n";

        let full = format!("{header}{oversized_body}{footer}");
        let mut redactor = StreamRedactor::for_command();
        let mut collected = String::new();

        for chunk in full.as_bytes().chunks(4096) {
            let emitted = redactor.push_bytes(chunk);
            assert!(
                !emitted.contains("MIIEpAIBAAKCAQEA")
                    && !emitted.contains("SECRET_PEM_TAIL_LINE_999"),
                "oversized PEM chunk leaked private key material: {emitted:?}"
            );
            collected.push_str(&emitted);
        }
        let (tail, hit) = redactor.finish();
        collected.push_str(&tail);

        assert!(hit);
        assert!(!collected.contains("MIIEpAIBAAKCAQEA"));
        assert!(!collected.contains("SECRET_PEM_TAIL_LINE_999"));
        assert!(!collected.contains("-----END RSA PRIVATE KEY-----"));
        assert_eq!(collected, "before-pem\n-----BEGIN [REDACTED]\nafter-pem\n");
    }
}
