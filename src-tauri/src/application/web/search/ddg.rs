//! Keyless `DuckDuckGo` HTML/Lite search backend (default backend, Stage 12 PR 6).
//!
//! - Primary: `POST https://html.duckduckgo.com/html/` with form `q=<query>&kl=wt-wt`
//! - Fallback: `POST https://lite.duckduckgo.com/lite/` on transport / 5xx / empty HTML
//! - Strips `/l/?uddg=<encoded_url>` redirect wrappers and re-validates every
//!   target URL through [`crate::application::web::safety::validate_fetch_url`].
//! - Detects `HTTP 202` / `403` / `429` and CAPTCHA / bot-challenge HTML
//!   (`anomaly.js`, `captcha`, `bots use duckduckgo`, etc.) as
//!   [`SearchError::RateLimitedOrBlocked`].

use url::Url;

use crate::application::agent::control::CancellationToken;
use crate::application::web::extract::decode_entities;
use crate::application::web::safety::validate_fetch_url;

use super::{
    normalize_and_filter_results, percent_decode, percent_encode, send_public_search_request,
    SearchBackend, SearchError, SearchHttpMethod, SearchHttpResponse, WebResult,
};

const DDG_HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const DDG_LITE_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";

/// Realistic non-empty browser-like `User-Agent` (research §7.2); never sends
/// Nexora cookies or credentials.
pub(crate) const DDG_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

const DDG_RATE_LIMIT_HINT: &str =
    "DuckDuckGo search was rate-limited or blocked by a bot challenge; retry later or switch to Tavily, Brave, Exa, or SearXNG in Settings -> Agent -> Web access";

const DDG_CAPTCHA_MARKERS: &[&str] = &[
    "anomaly.js",
    "captcha",
    "bots use duckduckgo",
    "please complete the following challenge",
    "are you a robot",
    "cc=botnet",
];

/// Keyless `DuckDuckGo` search backend.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DdgBackend;

impl DdgBackend {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl SearchBackend for DdgBackend {
    fn name(&self) -> &'static str {
        "ddg"
    }

    fn search(
        &self,
        query: &str,
        max_results: usize,
        token: &CancellationToken,
    ) -> Result<Vec<WebResult>, SearchError> {
        let form_body = format!("q={}&kl=wt-wt", percent_encode(query));
        let headers = [("Accept", "text/html,application/xhtml+xml;q=0.9")];

        let primary = send_public_search_request(
            DDG_HTML_ENDPOINT,
            SearchHttpMethod::PostForm,
            DDG_USER_AGENT,
            &headers,
            Some(&form_body),
            token,
        );

        match primary {
            Ok(resp) => {
                let parsed = handle_ddg_html_response(&resp, max_results);
                match parsed {
                    Ok(results) if !results.is_empty() || is_explicit_no_results(&resp.body) => {
                        return Ok(results);
                    }
                    Err(err @ SearchError::RateLimitedOrBlocked(_)) => return Err(err),
                    Ok(_) | Err(_) => {
                        // Fall through to lite endpoint when HTML yielded nothing parseable
                        // or returned transient 5xx.
                    }
                }
            }
            Err(SearchError::Cancelled) => return Err(SearchError::Cancelled),
            Err(_) => {
                // Fall through to lite endpoint on transport/timeout errors.
            }
        }

        let lite_resp = send_public_search_request(
            DDG_LITE_ENDPOINT,
            SearchHttpMethod::PostForm,
            DDG_USER_AGENT,
            &headers,
            Some(&form_body),
            token,
        )?;
        handle_ddg_lite_response(&lite_resp, max_results)
    }
}

/// Detect HTTP 202/403/429 or bot/CAPTCHA markers in a `DuckDuckGo` response.
#[must_use]
pub(crate) fn is_ddg_rate_limited_or_blocked(status: u16, body: &str) -> bool {
    if matches!(status, 202 | 403 | 429) {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    DDG_CAPTCHA_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

fn is_explicit_no_results(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("no results.")
        || lower.contains("no-results")
        || lower.contains("no more results")
}

/// Classify and parse a `html.duckduckgo.com/html/` HTTP response.
pub(crate) fn handle_ddg_html_response(
    resp: &SearchHttpResponse,
    max_results: usize,
) -> Result<Vec<WebResult>, SearchError> {
    if is_ddg_rate_limited_or_blocked(resp.status, &resp.body) {
        return Err(SearchError::RateLimitedOrBlocked(DDG_RATE_LIMIT_HINT));
    }
    if !(200..300).contains(&resp.status) {
        return Err(SearchError::HttpStatus {
            status: resp.status,
        });
    }
    let raw = parse_ddg_html(&resp.body);
    Ok(normalize_and_filter_results(raw, max_results))
}

/// Classify and parse a `lite.duckduckgo.com/lite/` HTTP response.
pub(crate) fn handle_ddg_lite_response(
    resp: &SearchHttpResponse,
    max_results: usize,
) -> Result<Vec<WebResult>, SearchError> {
    if is_ddg_rate_limited_or_blocked(resp.status, &resp.body) {
        return Err(SearchError::RateLimitedOrBlocked(DDG_RATE_LIMIT_HINT));
    }
    if !(200..300).contains(&resp.status) {
        return Err(SearchError::HttpStatus {
            status: resp.status,
        });
    }
    let raw = parse_ddg_lite_html(&resp.body);
    Ok(normalize_and_filter_results(raw, max_results))
}

/// Strip `DuckDuckGo` `/l/?uddg=<encoded_url>` redirect wrappers and re-validate
/// the extracted target URL through [`validate_fetch_url`].
#[must_use]
pub(crate) fn extract_and_validate_ddg_url(raw_href: &str) -> Option<String> {
    let decoded_href = decode_entities(raw_href).trim().to_string();
    if decoded_href.is_empty() {
        return None;
    }

    let candidate = if let Some(unwrapped) = unwrap_ddg_redirect(&decoded_href) {
        unwrapped
    } else if decoded_href.starts_with("//") {
        format!("https:{decoded_href}")
    } else {
        decoded_href
    };

    validate_fetch_url(&candidate)
        .ok()
        .map(|v| v.as_url().as_str().to_string())
}

fn unwrap_ddg_redirect(href: &str) -> Option<String> {
    let normalized = if href.starts_with("//") {
        format!("https:{href}")
    } else if href.starts_with('/') {
        format!("https://duckduckgo.com{href}")
    } else {
        href.to_string()
    };
    let parsed = Url::parse(&normalized).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let is_ddg_host = host == "duckduckgo.com" || host.ends_with(".duckduckgo.com");
    if !is_ddg_host || !parsed.path().starts_with("/l/") {
        return None;
    }
    for (k, v) in parsed.query_pairs() {
        if k == "uddg" {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    // Fallback raw query parse if `query_pairs` missed anything unusual.
    let query = parsed.query()?;
    for pair in query.split('&') {
        if let Some(encoded) = pair.strip_prefix("uddg=") {
            return percent_decode(encoded).filter(|s| !s.trim().is_empty());
        }
    }
    None
}

/// Parse `https://html.duckduckgo.com/html/` HTML into raw [`WebResult`] items
/// (each URL already stripped of `/l/?uddg=` and SSRF-validated).
#[must_use]
pub(crate) fn parse_ddg_html(html: &str) -> Vec<WebResult> {
    let mut results = Vec::new();
    let mut cursor = 0;

    while let Some((anchor_open_start, anchor_open_end, tag_str)) =
        find_next_tag_with_class(html, cursor, "a", "result__a")
    {
        let href = extract_attr(tag_str, "href").unwrap_or_default();
        let Some(close_rel) = find_closing_tag(&html[anchor_open_end..], "a") else {
            break;
        };
        let title_raw = &html[anchor_open_end..anchor_open_end + close_rel];
        let title = strip_tags_and_decode(title_raw);
        let after_anchor = anchor_open_end + close_rel + 4;

        // Bound snippet search before the next `result__a` link so we never
        // attribute a later result's snippet to an earlier result without one.
        let next_anchor_start = find_next_tag_with_class(html, after_anchor, "a", "result__a")
            .map_or(html.len(), |(start, _, _)| start);
        let region = &html[after_anchor..next_anchor_start];
        let snippet = extract_first_element_by_class(region, "result__snippet").unwrap_or_default();

        if let Some(url) = extract_and_validate_ddg_url(&href) {
            results.push(WebResult {
                title,
                url,
                snippet,
            });
        }

        cursor = after_anchor.max(anchor_open_start + 1);
    }

    results
}

/// Parse `https://lite.duckduckgo.com/lite/` HTML into raw [`WebResult`] items.
#[must_use]
pub(crate) fn parse_ddg_lite_html(html: &str) -> Vec<WebResult> {
    let mut results = Vec::new();
    let mut cursor = 0;

    while let Some((anchor_open_start, anchor_open_end, tag_str)) =
        find_next_tag_with_class(html, cursor, "a", "result-link")
    {
        let href = extract_attr(tag_str, "href").unwrap_or_default();
        let Some(close_rel) = find_closing_tag(&html[anchor_open_end..], "a") else {
            break;
        };
        let title_raw = &html[anchor_open_end..anchor_open_end + close_rel];
        let title = strip_tags_and_decode(title_raw);
        let after_anchor = anchor_open_end + close_rel + 4;

        let next_anchor_start = find_next_tag_with_class(html, after_anchor, "a", "result-link")
            .map_or(html.len(), |(start, _, _)| start);
        let region = &html[after_anchor..next_anchor_start];
        let snippet = extract_first_element_by_class(region, "result-snippet").unwrap_or_default();

        if let Some(url) = extract_and_validate_ddg_url(&href) {
            results.push(WebResult {
                title,
                url,
                snippet,
            });
        }

        cursor = after_anchor.max(anchor_open_start + 1);
    }

    results
}

fn find_next_tag_with_class<'a>(
    html: &'a str,
    from: usize,
    tag_name: &str,
    required_class: &str,
) -> Option<(usize, usize, &'a str)> {
    let bytes = html.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        let lt_rel = html[i..].find('<')?;
        let start = i + lt_rel;
        let end = find_tag_end(html, start)?;
        let tag_slice = &html[start..=end];
        if tag_matches_name(tag_slice, tag_name) {
            if let Some(class_val) = extract_attr(tag_slice, "class") {
                if class_val
                    .split_ascii_whitespace()
                    .any(|token| token == required_class)
                {
                    return Some((start, end + 1, tag_slice));
                }
            }
        }
        i = end + 1;
    }
    None
}

fn extract_first_element_by_class(html: &str, required_class: &str) -> Option<String> {
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let lt_rel = html[i..].find('<')?;
        let start = i + lt_rel;
        let end = find_tag_end(html, start)?;
        let tag_slice = &html[start..=end];
        if !tag_slice.starts_with("</") && !tag_slice.starts_with("<!") {
            if let Some(class_val) = extract_attr(tag_slice, "class") {
                if class_val
                    .split_ascii_whitespace()
                    .any(|token| token == required_class)
                {
                    let name = parse_tag_name(tag_slice)?;
                    let inner_start = end + 1;
                    let close_rel = find_closing_tag(&html[inner_start..], &name)?;
                    return Some(strip_tags_and_decode(
                        &html[inner_start..inner_start + close_rel],
                    ));
                }
            }
        }
        i = end + 1;
    }
    None
}

fn find_tag_end(html: &str, start: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let max_end = (start + 8192).min(bytes.len());
    let mut quote: Option<u8> = None;
    let mut i = start + 1;
    while i < max_end {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            Some(_) | None => {}
        }
        i += 1;
    }
    None
}

fn parse_tag_name(tag_slice: &str) -> Option<String> {
    let inner = tag_slice.strip_prefix('<')?.trim_start();
    let name: String = inner
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name.to_ascii_lowercase())
    }
}

fn tag_matches_name(tag_slice: &str, expected: &str) -> bool {
    parse_tag_name(tag_slice).is_some_and(|name| name == expected)
}

fn find_closing_tag(slice: &str, tag_name: &str) -> Option<usize> {
    let needle = format!("</{tag_name}");
    let lower = slice.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(&needle) {
        let pos = search_from + rel;
        let after = pos + needle.len();
        if let Some(next_byte) = lower.as_bytes().get(after) {
            if *next_byte == b'>' || next_byte.is_ascii_whitespace() {
                return Some(pos);
            }
        }
        search_from = after;
    }
    None
}

fn extract_attr(tag_slice: &str, attr_name: &str) -> Option<String> {
    let inner = tag_slice
        .strip_prefix('<')?
        .strip_suffix('>')?
        .trim_end_matches('/');
    // Skip tag name.
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'>'
        {
            i += 1;
        }
        let name = &inner[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let val_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                let val = &inner[val_start..i];
                if i < bytes.len() {
                    i += 1;
                }
                Some(val.to_string())
            } else {
                let val_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                Some(inner[val_start..i].to_string())
            }
        } else {
            None
        };
        if name.eq_ignore_ascii_case(attr_name) {
            return value;
        }
    }
    None
}

fn strip_tags_and_decode(fragment: &str) -> String {
    let mut plain = String::with_capacity(fragment.len());
    let mut in_tag = false;
    for ch in fragment.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => plain.push(ch),
            _ => {}
        }
    }
    decode_entities(&plain)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ddg_html_strips_uddg_redirect_and_revalidates_targets() {
        let html = r#"
        <html><body>
          <div class="result results_links results_links_deep web-result">
            <h2 class="result__title">
              <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F&amp;rut=abc">The <b>Rust</b> Book &amp; Guide</a>
            </h2>
            <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F">Learn <b>Rust</b> programming cleanly.</a>
          </div>
          <div class="result web-result">
            <a class="result__a" href="/l/?uddg=http%3A%2F%2F127.0.0.1%3A8080%2Fsecret">SSRF Trap</a>
            <div class="result__snippet">Should be dropped!</div>
          </div>
          <div class="result web-result">
            <a class="result__a" href="/l/?uddg=javascript%3Aalert(1)">XSS Trap</a>
            <div class="result__snippet">Should also be dropped!</div>
          </div>
          <div class="result web-result">
            <a class="result__a" href="https://example.org/docs">Direct Result</a>
            <span class="result__snippet">Direct snippet text.</span>
          </div>
        </body></html>
        "#;

        let resp = SearchHttpResponse {
            status: 200,
            body: html.to_string(),
        };
        let results = handle_ddg_html_response(&resp, 5).expect("parse should succeed");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "The Rust Book & Guide");
        assert_eq!(results[0].url, "https://doc.rust-lang.org/book/");
        assert_eq!(results[0].snippet, "Learn Rust programming cleanly.");
        assert_eq!(results[1].title, "Direct Result");
        assert_eq!(results[1].url, "https://example.org/docs");
        assert_eq!(results[1].snippet, "Direct snippet text.");
    }

    #[test]
    fn parses_ddg_lite_fallback_html() {
        let lite_html = r#"
        <html><body><table>
          <tr><td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fserde.rs%2F" class="result-link">Serde</a></td></tr>
          <tr><td class="result-snippet">Serialization framework for <b>Rust</b>.</td></tr>
          <tr><td><a rel="nofollow" href="/l/?uddg=http%3A%2F%2F169.254.169.254%2Flatest" class="result-link">Metadata</a></td></tr>
          <tr><td class="result-snippet">Must be dropped.</td></tr>
        </table></body></html>
        "#;
        let resp = SearchHttpResponse {
            status: 200,
            body: lite_html.to_string(),
        };
        let results = handle_ddg_lite_response(&resp, 5).expect("lite parse");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Serde");
        assert_eq!(results[0].url, "https://serde.rs/");
        assert_eq!(results[0].snippet, "Serialization framework for Rust.");
    }

    #[test]
    fn detects_rate_limit_statuses_and_captcha_html_as_blocked() {
        for status in [202u16, 403, 429] {
            let resp = SearchHttpResponse {
                status,
                body: String::new(),
            };
            assert!(
                matches!(
                    handle_ddg_html_response(&resp, 5),
                    Err(SearchError::RateLimitedOrBlocked(_))
                ),
                "status {status} must map to RateLimitedOrBlocked"
            );
        }

        for body in [
            "<html><script src='/anomaly.js'></script></html>",
            "<html><body>Unfortunately, bots use DuckDuckGo too.</body></html>",
            "<html><body>Please complete the CAPTCHA challenge.</body></html>",
        ] {
            let resp = SearchHttpResponse {
                status: 200,
                body: body.to_string(),
            };
            assert!(
                matches!(
                    handle_ddg_html_response(&resp, 5),
                    Err(SearchError::RateLimitedOrBlocked(_))
                ),
                "captcha body {body:?} must not be treated as empty results"
            );
        }
    }

    #[test]
    fn extract_and_validate_ddg_url_rejects_private_and_non_http() {
        assert_eq!(
            extract_and_validate_ddg_url(
                "/l/?uddg=https%3A%2F%2Fexample.com%2Fpath%3Fq%3D1&rut=deadbeef"
            ),
            Some("https://example.com/path?q=1".to_string())
        );
        assert_eq!(
            extract_and_validate_ddg_url("/l/?uddg=http%3A%2F%2F127.0.0.1%2F"),
            None
        );
        assert_eq!(
            extract_and_validate_ddg_url("/l/?uddg=http%3A%2F%2F10.0.0.1%2Finternal"),
            None
        );
        assert_eq!(
            extract_and_validate_ddg_url("/l/?uddg=file%3A%2F%2F%2Fetc%2Fpasswd"),
            None
        );
    }
}
