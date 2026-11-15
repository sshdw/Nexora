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
    let (blocked, pem_hit) = redact_pem_blocks(text, marker);
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
        let flush_end = safe_flush_end(&self.pending);
        if flush_end == 0 {
            return String::new();
        }
        let prefix: String = self.pending.drain(..flush_end).collect();
        let (scrubbed, hit) = redact_with_marker(&prefix, self.marker);
        self.any_redacted |= hit;
        scrubbed
    }

    /// Flushes any buffered trailing bytes/lines at EOF and returns the final
    /// redacted chunk paired with whether any secret was redacted across the
    /// whole stream.
    pub(crate) fn finish(&mut self) -> (String, bool) {
        if self.utf8_tail_len > 0 {
            self.pending.push('\u{FFFD}');
            self.utf8_tail_len = 0;
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

fn safe_flush_end(pending: &str) -> usize {
    if pending.is_empty() {
        return 0;
    }

    // Hold back any open PEM block until `-----END ...-----\n` arrives or the
    // fail-closed hold cap trips.
    let bytes = pending.as_bytes();
    let mut scan = 0;
    while let Some(begin) = find_ascii_ci(bytes, b"-----begin", scan) {
        if let Some(end_pos) = find_ascii_ci(bytes, b"-----end", begin + 10) {
            if let Some(nl_rel) = pending[end_pos..].find('\n') {
                scan = end_pos + nl_rel + 1;
                continue;
            }
        }
        if pending.len() - begin >= MAX_PEM_BLOCK_HOLD {
            return pending.len();
        }
        return begin;
    }

    let baseline = pending.rfind('\n').map_or(0, |nl| nl + 1);
    let tail = &pending[baseline..];
    if tail.is_empty() {
        return baseline;
    }

    if tail.len() >= MAX_PARTIAL_LINE_HOLD {
        let target = tail.len().saturating_sub(OVERSIZED_LINE_TAIL_KEEP);
        let safe_in_tail = tail[..target]
            .rfind([' ', '\t'])
            .map_or_else(|| floor_char_boundary(tail, target), |ws| ws + 1);
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

fn redact_pem_blocks(text: &str, marker: &str) -> (String, bool) {
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
        scrubbed.push_str("-----BEGIN ");
        scrubbed.push_str(marker);
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
}
