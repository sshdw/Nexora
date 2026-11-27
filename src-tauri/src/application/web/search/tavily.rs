//! Tavily search backend (opt-in, API key stored exclusively in OS keyring).

use serde::Deserialize;
use serde_json::json;

use crate::application::agent::control::CancellationToken;
use crate::application::web::WEB_FETCH_USER_AGENT;

use super::{
    clamp_max_results, normalize_and_filter_results, send_public_search_request, SearchBackend,
    SearchError, SearchHttpMethod, SearchHttpResponse, WebResult,
};

const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

pub(crate) struct TavilyBackend {
    api_key: String,
}

impl TavilyBackend {
    #[must_use]
    pub(crate) fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[derive(Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyItem>,
}

#[derive(Deserialize)]
struct TavilyItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

impl SearchBackend for TavilyBackend {
    fn name(&self) -> &'static str {
        "tavily"
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
            "max_results": cap,
            "search_depth": "basic",
            "include_answer": false,
            "include_raw_content": false,
        })
        .to_string();
        let auth_header = format!("Bearer {}", self.api_key);
        let headers = [
            ("Accept", "application/json"),
            ("Authorization", auth_header.as_str()),
        ];

        let resp = send_public_search_request(
            TAVILY_ENDPOINT,
            SearchHttpMethod::PostJson,
            WEB_FETCH_USER_AGENT,
            &headers,
            Some(&payload),
            token,
        )?;
        parse_tavily_response(&resp, cap)
    }
}

pub(crate) fn parse_tavily_response(
    resp: &SearchHttpResponse,
    max_results: usize,
) -> Result<Vec<WebResult>, SearchError> {
    if resp.status == 429 {
        return Err(SearchError::RateLimitedOrBlocked(
            "Tavily search rate limit exceeded; retry later or switch backend",
        ));
    }
    if matches!(resp.status, 401 | 403) {
        return Err(SearchError::MissingApiKey(
            "Tavily search rejected the configured API key",
        ));
    }
    if !(200..300).contains(&resp.status) {
        return Err(SearchError::HttpStatus {
            status: resp.status,
        });
    }
    let parsed: TavilyResponse = serde_json::from_str(&resp.body)
        .map_err(|_| SearchError::ParseFailed("Tavily search returned invalid JSON"))?;
    let raw = parsed
        .results
        .into_iter()
        .map(|item| WebResult {
            title: item.title,
            url: item.url,
            snippet: item.content,
        })
        .collect();
    Ok(normalize_and_filter_results(raw, max_results))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tavily_json_and_filters_unsafe_urls() {
        let resp = SearchHttpResponse {
            status: 200,
            body: r#"{
              "results": [
                {"title": "Rust", "url": "https://www.rust-lang.org/", "content": "Fast systems language"},
                {"title": "Local", "url": "http://127.0.0.1/secret", "content": "drop me"}
              ]
            }"#
            .to_string(),
        };
        let results = parse_tavily_response(&resp, 5).expect("parse tavily");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[0].snippet, "Fast systems language");
    }

    #[test]
    fn maps_tavily_auth_and_rate_limit_errors_without_secrets() {
        let unauth = SearchHttpResponse {
            status: 401,
            body: "invalid tvly-secret-123".to_string(),
        };
        assert!(matches!(
            parse_tavily_response(&unauth, 5),
            Err(SearchError::MissingApiKey(_))
        ));

        let limited = SearchHttpResponse {
            status: 429,
            body: "slow down".to_string(),
        };
        assert!(matches!(
            parse_tavily_response(&limited, 5),
            Err(SearchError::RateLimitedOrBlocked(_))
        ));
    }
}
