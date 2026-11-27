//! Brave Search API backend (opt-in, API key stored exclusively in OS keyring).

use serde::Deserialize;

use crate::application::agent::control::CancellationToken;
use crate::application::web::WEB_FETCH_USER_AGENT;

use super::{
    clamp_max_results, normalize_and_filter_results, percent_encode, send_public_search_request,
    SearchBackend, SearchError, SearchHttpMethod, SearchHttpResponse, WebResult,
};

const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

pub(crate) struct BraveBackend {
    api_key: String,
}

impl BraveBackend {
    #[must_use]
    pub(crate) fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[derive(Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWebSection>,
}

#[derive(Deserialize)]
struct BraveWebSection {
    #[serde(default)]
    results: Vec<BraveItem>,
}

#[derive(Deserialize)]
struct BraveItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

impl SearchBackend for BraveBackend {
    fn name(&self) -> &'static str {
        "brave"
    }

    fn search(
        &self,
        query: &str,
        max_results: usize,
        token: &CancellationToken,
    ) -> Result<Vec<WebResult>, SearchError> {
        let cap = clamp_max_results(max_results);
        let url = format!("{BRAVE_ENDPOINT}?q={}&count={cap}", percent_encode(query));
        let headers = [
            ("Accept", "application/json"),
            ("X-Subscription-Token", self.api_key.as_str()),
        ];

        let resp = send_public_search_request(
            &url,
            SearchHttpMethod::Get,
            WEB_FETCH_USER_AGENT,
            &headers,
            None,
            token,
        )?;
        parse_brave_response(&resp, cap)
    }
}

pub(crate) fn parse_brave_response(
    resp: &SearchHttpResponse,
    max_results: usize,
) -> Result<Vec<WebResult>, SearchError> {
    if resp.status == 429 {
        return Err(SearchError::RateLimitedOrBlocked(
            "Brave search rate limit exceeded; retry later or switch backend",
        ));
    }
    if matches!(resp.status, 401 | 403) {
        return Err(SearchError::MissingApiKey(
            "Brave search rejected the configured API key",
        ));
    }
    if !(200..300).contains(&resp.status) {
        return Err(SearchError::HttpStatus {
            status: resp.status,
        });
    }
    let parsed: BraveResponse = serde_json::from_str(&resp.body)
        .map_err(|_| SearchError::ParseFailed("Brave search returned invalid JSON"))?;
    let raw = parsed
        .web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .map(|item| WebResult {
            title: item.title,
            url: item.url,
            snippet: item.description,
        })
        .collect();
    Ok(normalize_and_filter_results(raw, max_results))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_brave_json_and_respects_cap() {
        let resp = SearchHttpResponse {
            status: 200,
            body: r#"{
              "web": {
                "results": [
                  {"title": "One", "url": "https://example.com/1", "description": "First"},
                  {"title": "Two", "url": "https://example.com/2", "description": "Second"},
                  {"title": "Three", "url": "https://example.com/3", "description": "Third"}
                ]
              }
            }"#
            .to_string(),
        };
        let results = parse_brave_response(&resp, 2).expect("parse brave");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "One");
        assert_eq!(results[1].url, "https://example.com/2");
    }
}
