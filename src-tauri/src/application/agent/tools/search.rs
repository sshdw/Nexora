//! Workspace file-content search: the regex-lite engine and its walker.
//!
//! Split from `executor.rs` (AC-207): `search_files`'s pure matching layer —
//! the bounded pattern subset ([`LiteRegex`]), the per-file scanner, and the
//! workspace walk — lives here; the registry's dispatch stays in
//! [`super::executor`].

use std::path::Path;

use super::executor::ToolError;
use super::path_safety::is_within_workspace;

// ---------------------------------------------------------------------------
// Regex-lite search (`search_files`)
// ---------------------------------------------------------------------------

/// Default/maximum matches returned by `search_files`.
pub(super) const SEARCH_DEFAULT_MAX_MATCHES: usize = 50;
pub(super) const SEARCH_HARD_CAP_MATCHES: usize = 200;
/// Patterns longer than this are rejected rather than compiled.
pub(super) const MAX_SEARCH_PATTERN_BYTES: usize = 10 * 1024;
/// Files larger than this are skipped (binary-ish / unbounded-read guard).
pub(super) const MAX_SEARCH_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Bytes sniffed for NUL when deciding a file is binary.
pub(super) const BINARY_SNIFF_BYTES: usize = 8000;
/// Display budget for one hit line before middle truncation.
pub(super) const SEARCH_LINE_BUDGET_BYTES: usize = 1000;
pub(super) const SEARCH_LINE_EDGE_BYTES: usize = 500;
/// Total atom tests for one line before the match gives up (hang guard for
/// pathological quantifiers on huge single-line files).
pub(super) const SEARCH_MATCH_STEP_BUDGET: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchAtom {
    Lit(char),
    Dot,
    Digit,
    NotDigit,
    Word,
    NotWord,
    Space,
    NotSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchQuant {
    One,
    ZeroMore,
    OneMore,
    ZeroOne,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SearchToken {
    atom: SearchAtom,
    quant: SearchQuant,
}

/// Regex-lite pattern: literals, `.`, `*`/`+`/`?` on the preceding element,
/// `^`/`$` anchors, `\d \D \w \W \s \S`. Every other metacharacter
/// (`( ) [ ] { } |`) matches literally. Matching is case-sensitive.
#[derive(Debug, Clone)]
pub(super) struct LiteRegex {
    anchored_start: bool,
    anchored_end: bool,
    tokens: Vec<SearchToken>,
}

impl LiteRegex {
    pub(super) fn compile(pattern: &str) -> Self {
        let mut anchored_start = false;
        let mut body = pattern;
        if let Some(rest) = body.strip_prefix('^') {
            anchored_start = true;
            body = rest;
        }
        let mut anchored_end = false;
        let mut tokens: Vec<SearchToken> = Vec::new();
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    let atom = match chars.next() {
                        None => SearchAtom::Lit('\\'),
                        Some('d') => SearchAtom::Digit,
                        Some('D') => SearchAtom::NotDigit,
                        Some('w') => SearchAtom::Word,
                        Some('W') => SearchAtom::NotWord,
                        Some('s') => SearchAtom::Space,
                        Some('S') => SearchAtom::NotSpace,
                        Some(other) => SearchAtom::Lit(other),
                    };
                    tokens.push(SearchToken {
                        atom,
                        quant: SearchQuant::One,
                    });
                }
                '.' => tokens.push(SearchToken {
                    atom: SearchAtom::Dot,
                    quant: SearchQuant::One,
                }),
                // A trailing unescaped `$` anchors; anywhere else it is literal.
                // (`\$` is consumed by the escape arm above, so it stays literal.)
                '$' if chars.peek().is_none() => anchored_end = true,
                '*' | '+' | '?' => {
                    if let Some(last) = tokens.last_mut() {
                        last.quant = match c {
                            '*' => SearchQuant::ZeroMore,
                            '+' => SearchQuant::OneMore,
                            _ => SearchQuant::ZeroOne,
                        };
                    } else {
                        tokens.push(SearchToken {
                            atom: SearchAtom::Lit(c),
                            quant: SearchQuant::One,
                        });
                    }
                }
                other => tokens.push(SearchToken {
                    atom: SearchAtom::Lit(other),
                    quant: SearchQuant::One,
                }),
            }
        }
        Self {
            anchored_start,
            anchored_end,
            tokens,
        }
    }

    fn is_match(&self, line: &str) -> bool {
        let chars: Vec<char> = line.chars().collect();
        if self.anchored_start {
            let mut budget = SEARCH_MATCH_STEP_BUDGET;
            self.match_at(&chars, 0, &mut budget)
                .is_some_and(|end| !self.anchored_end || end == chars.len())
        } else {
            (0..=chars.len()).any(|start| {
                let mut budget = SEARCH_MATCH_STEP_BUDGET;
                self.match_at(&chars, start, &mut budget)
                    .is_some_and(|end| !self.anchored_end || end == chars.len())
            })
        }
    }

    fn match_at(&self, chars: &[char], start: usize, budget: &mut usize) -> Option<usize> {
        self.match_tokens(0, chars, start, budget)
    }

    fn match_tokens(
        &self,
        token_index: usize,
        chars: &[char],
        char_index: usize,
        budget: &mut usize,
    ) -> Option<usize> {
        if token_index == self.tokens.len() {
            return Some(char_index);
        }
        let token = &self.tokens[token_index];
        match token.quant {
            SearchQuant::One => {
                if char_index < chars.len()
                    && Self::atom_matches(token.atom, chars, char_index, budget)
                {
                    self.match_tokens(token_index + 1, chars, char_index + 1, budget)
                } else {
                    None
                }
            }
            SearchQuant::ZeroOne => {
                if char_index < chars.len()
                    && Self::atom_matches(token.atom, chars, char_index, budget)
                {
                    if let Some(end) =
                        self.match_tokens(token_index + 1, chars, char_index + 1, budget)
                    {
                        return Some(end);
                    }
                }
                self.match_tokens(token_index + 1, chars, char_index, budget)
            }
            SearchQuant::ZeroMore | SearchQuant::OneMore => {
                let minimum = usize::from(token.quant == SearchQuant::OneMore);
                let mut run = 0;
                while char_index + run < chars.len()
                    && Self::atom_matches(token.atom, chars, char_index + run, budget)
                {
                    run += 1;
                }
                let mut take = run;
                loop {
                    if take >= minimum {
                        if let Some(end) =
                            self.match_tokens(token_index + 1, chars, char_index + take, budget)
                        {
                            return Some(end);
                        }
                    }
                    if take == 0 {
                        break;
                    }
                    take -= 1;
                    if take < minimum {
                        break;
                    }
                }
                None
            }
        }
    }

    fn atom_matches(atom: SearchAtom, chars: &[char], index: usize, budget: &mut usize) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let c = chars[index];
        match atom {
            SearchAtom::Lit(literal) => literal == c,
            SearchAtom::Dot => true,
            SearchAtom::Digit => c.is_ascii_digit(),
            SearchAtom::NotDigit => !c.is_ascii_digit(),
            SearchAtom::Word => c.is_alphanumeric() || c == '_',
            SearchAtom::NotWord => !(c.is_alphanumeric() || c == '_'),
            SearchAtom::Space => c.is_whitespace(),
            SearchAtom::NotSpace => !c.is_whitespace(),
        }
    }
}

pub(super) fn walk_search(
    dir: &Path,
    workspace_root: &Path,
    ignore: &crate::application::project_dir::IgnoreRules,
    regex: &LiteRegex,
    max_matches: usize,
    hits: &mut Vec<String>,
    capped: &mut bool,
) -> Result<(), ToolError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| ToolError::Io(format!("failed to read directory: {e}")))?;
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| ToolError::Io(format!("failed to read entry: {e}")))?;
        // Symbolic links are never followed (symlink escapes stay excluded).
        if entry
            .file_type()
            .map_err(|e| ToolError::Io(format!("failed to get file type: {e}")))?
            .is_symlink()
        {
            continue;
        }
        paths.push(entry.path());
    }
    paths.sort();
    for path in paths {
        if *capped {
            return Ok(());
        }
        // Defense against races/odd mounts: never leave the workspace.
        if !is_within_workspace(workspace_root, &path) {
            continue;
        }
        // Workspace ignore contract: ignored paths (including `.nexora/`)
        // never contribute matches.
        if crate::application::project_dir::is_excluded(workspace_root, &path, ignore) {
            continue;
        }
        // TOCTOU guard: the entry may have been swapped for a link after the
        // listing above; never follow a path whose canonical target escaped.
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        if let Ok(canon) = path.canonicalize() {
            if !is_within_workspace(workspace_root, &canon) {
                continue;
            }
        }
        if path.is_dir() {
            walk_search(
                &path,
                workspace_root,
                ignore,
                regex,
                max_matches,
                hits,
                capped,
            )?;
        } else if path.is_file() {
            search_one_file(&path, workspace_root, regex, max_matches, hits, capped)?;
        }
    }
    Ok(())
}

pub(super) fn search_one_file(
    path: &Path,
    workspace_root: &Path,
    regex: &LiteRegex,
    max_matches: usize,
    hits: &mut Vec<String>,
    capped: &mut bool,
) -> Result<(), ToolError> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| ToolError::Io(format!("failed to read file metadata: {e}")))?;
    if metadata.len() > MAX_SEARCH_FILE_BYTES {
        return Ok(());
    }
    let bytes =
        std::fs::read(path).map_err(|e| ToolError::Io(format!("failed to read file: {e}")))?;
    if bytes.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0) {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&bytes);
    let relative = path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    for (index, line) in text.lines().enumerate() {
        if regex.is_match(line) {
            if hits.len() >= max_matches {
                *capped = true;
                return Ok(());
            }
            hits.push(format!(
                "{}:{}:{}",
                relative,
                index + 1,
                shorten_hit_line(line)
            ));
        }
    }
    Ok(())
}

/// Middle-truncate one hit line, keeping both edges with a notice.
pub(super) fn shorten_hit_line(line: &str) -> String {
    if line.len() <= SEARCH_LINE_BUDGET_BYTES {
        return line.to_string();
    }
    let head_end = floor_char_boundary(line, SEARCH_LINE_EDGE_BYTES);
    let tail_start = ceil_char_boundary(line, line.len().saturating_sub(SEARCH_LINE_EDGE_BYTES));
    format!(
        "{}... [line truncated, {} bytes, showing first {} and last {} bytes] ...{}",
        &line[..head_end],
        line.len(),
        head_end,
        line.len() - tail_start,
        &line[tail_start..]
    )
}

pub(super) fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    index = std::cmp::min(index, s.len());
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub(super) fn ceil_char_boundary(s: &str, mut index: usize) -> usize {
    index = std::cmp::min(index, s.len());
    while index < s.len() && !s.is_char_boundary(index) {
        index += 1;
    }
    index
}
