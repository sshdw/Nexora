//! Exa Search API backend (opt-in, API key stored exclusively in OS keyring).

use serde::Deserialize;
use serde_json::json;

use crate::application::agent::control::CancellationToken;
use crate::application::web::WEB_FETCH_USER_AGENT;

use super::{
    clamp_max_results, normalize_and_filter_results, send_public_search_request, SearchBackend,
    SearchError, SearchHttpMethod, SearchHttpResponse, WebResult,
};

const EXA_ENDPOINT: &str = "https://api.exa.ai/search";

pub(crate) struct ExaBackend {
    api_key: String,
}

impl ExaBackend {
    #[must_use]
    pub(crate) fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[derive(Deserialize)]
struct ExaResponse {
    #[serde(default)]
    results: Vec<ExaItem>,
}

#[derive(Deserialize)]
struct ExaItem {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    highlights: Vec<String>,
}

impl SearchBackend for ExaBackend {
    fn name(&self) -> &'static str {
        "exa"
    }

    fn search(
        &self,
        query: &str,
        max_results: usize,
        token: &CancellationToken,
    ) -> Result<Vec<WebResult>, SearchError> {
        let cap = clamp_max_results(max_results);
        let payload = json!({
            "query": query,
            "numResults": cap,
            "contents": {
                "highlights": true
            }
        })
        .to_string();
        let headers = [
            ("Accept", "application/json"),
            ("x-api-key", self.api_key.as_str()),
        ];

        let resp = send_public_search_request(
            EXA_ENDPOINT,
            SearchHttpMethod::PostJson,
            WEB_FETCH_USER_AGENT,
            &headers,
            Some(&payload),
            token,
        )?;
        parse_exa_response(&resp, cap)
    }
}

pub(crate) fn parse_exa_response(
    resp: &SearchHttpResponse,
    max_results: usize,
) -> Result<Vec<WebResult>, SearchError> {
    if resp.status == 429 {
        return Err(SearchError::RateLimitedOrBlocked(
            "Exa search rate limit exceeded; retry later or switch backend",
        ));
    }
    if matches!(resp.status, 401 | 403) {
        return Err(SearchError::MissingApiKey(
            "Exa search rejected the configured API key",
        ));
    }
    if !(200..300).contains(&resp.status) {
        return Err(SearchError::HttpStatus {
            status: resp.status,
        });
    }
    let parsed: ExaResponse = serde_json::from_str(&resp.body)
        .map_err(|_| SearchError::ParseFailed("Exa search returned invalid JSON"))?;
    let raw = parsed
        .results
        .into_iter()
        .map(|item| {
            let snippet = if item.highlights.is_empty() {
                item.text.unwrap_or_default()
            } else {
                item.highlights.join(" ")
            };
            WebResult {
                title: item.title.unwrap_or_default(),
                url: item.url,
                snippet,
            }
        })
        .collect();
    Ok(normalize_and_filter_results(raw, max_results))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exa_highlights_and_text_fallback() {
        let resp = SearchHttpResponse {
            status: 200,
            body: r#"{
              "results": [
                {"title": "Exa One", "url": "https://example.org/a", "highlights": ["alpha", "beta"]},
                {"title": "Exa Two", "url": "https://example.org/b", "text": "plain text"}
              ]
            }"#
            .to_string(),
        };
        let results = parse_exa_response(&resp, 5).expect("parse exa");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].snippet, "alpha beta");
        assert_eq!(results[1].snippet, "plain text");
    }
}
