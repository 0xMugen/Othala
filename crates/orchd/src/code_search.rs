use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SearchProvider {
    Sourcegraph { endpoint: String },
    GitHubCode { token: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeSearchConfig {
    pub provider: SearchProvider,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    pub language_filter: Option<String>,
}

impl Default for CodeSearchConfig {
    fn default() -> Self {
        Self {
            provider: SearchProvider::Sourcegraph {
                endpoint: "https://sourcegraph.example/api/graphql".to_string(),
            },
            max_results: default_max_results(),
            timeout_secs: default_timeout_secs(),
            language_filter: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeSearchQuery {
    pub query: String,
    pub language: Option<String>,
    pub repo_filter: Option<String>,
    pub file_filter: Option<String>,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeSearchResult {
    pub file_path: String,
    pub repo: String,
    pub line_number: u32,
    pub content: String,
    pub language: Option<String>,
    pub url: String,
}

pub struct CodeSearchClient {
    pub config: CodeSearchConfig,
}

impl CodeSearchClient {
    pub fn new(config: CodeSearchConfig) -> Self {
        Self { config }
    }

    pub fn build_sourcegraph_query(&self, query: &CodeSearchQuery) -> String {
        let mut parts = vec![query.query.clone()];

        if let Some(repo) = &query.repo_filter {
            parts.push(format!("repo:{repo}"));
        }

        if let Some(file) = &query.file_filter {
            parts.push(format!("file:{file}"));
        }

        if let Some(language) = query.language.as_ref().or(self.config.language_filter.as_ref()) {
            parts.push(format!("lang:{language}"));
        }

        let count = query.max_results.unwrap_or(self.config.max_results);
        parts.push(format!("count:{count}"));
        parts.join(" ")
    }

    pub fn build_github_query(&self, query: &CodeSearchQuery) -> String {
        let mut parts = vec![query.query.clone()];

        if let Some(repo) = &query.repo_filter {
            parts.push(format!("repo:{repo}"));
        }

        if let Some(file) = &query.file_filter {
            parts.push(format!("path:{file}"));
        }

        if let Some(language) = query.language.as_ref().or(self.config.language_filter.as_ref()) {
            parts.push(format!("language:{language}"));
        }

        parts.join(" ")
    }

    pub fn parse_sourcegraph_response(
        &self,
        json_str: &str,
    ) -> Result<Vec<CodeSearchResult>, SearchError> {
        let value: Value = serde_json::from_str(json_str)
            .map_err(|err| SearchError::ParseError(format!("invalid sourcegraph JSON: {err}")))?;

        if let Some(rate_limit_seconds) = value
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|errors| errors.first())
            .and_then(|first| first.get("extensions"))
            .and_then(|ext| ext.get("retryAfterSeconds"))
            .and_then(Value::as_u64)
        {
            return Err(SearchError::RateLimited(rate_limit_seconds));
        }

        let items = value
            .get("data")
            .and_then(|v| v.get("search"))
            .and_then(|v| v.get("results"))
            .and_then(|v| v.get("results"))
            .and_then(Value::as_array)
            .ok_or_else(|| SearchError::ParseError("missing sourcegraph results array".to_string()))?;

        let mut parsed = Vec::with_capacity(items.len());
        for item in items {
            let file_path = string_field(item, &["file", "path"])
                .or_else(|| string_field(item, &["path"]))
                .ok_or_else(|| SearchError::ParseError("missing sourcegraph file path".to_string()))?;

            let repo = string_field(item, &["repository", "name"])
                .or_else(|| string_field(item, &["repo"]))
                .unwrap_or_else(|| "unknown".to_string());

            let line_number = item
                .get("lineNumber")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0);

            let content = string_field(item, &["content"])
                .or_else(|| string_field(item, &["preview"]))
                .unwrap_or_default();

            let language = string_field(item, &["language"]);

            let url = string_field(item, &["url"]).unwrap_or_default();

            parsed.push(CodeSearchResult {
                file_path,
                repo,
                line_number,
                content,
                language,
                url,
            });
        }

        Ok(parsed)
    }

    pub fn parse_github_response(
        &self,
        json_str: &str,
    ) -> Result<Vec<CodeSearchResult>, SearchError> {
        let value: Value = serde_json::from_str(json_str)
            .map_err(|err| SearchError::ParseError(format!("invalid github JSON: {err}")))?;

        if value.get("message").and_then(Value::as_str) == Some("Requires authentication") {
            return Err(SearchError::AuthenticationRequired);
        }

        if let Some(reset_after) = value
            .get("rate_limit_reset")
            .and_then(Value::as_u64)
            .or_else(|| value.get("retry_after").and_then(Value::as_u64))
        {
            return Err(SearchError::RateLimited(reset_after));
        }

        let items = value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| SearchError::ParseError("missing github items array".to_string()))?;

        let mut parsed = Vec::with_capacity(items.len());
        for item in items {
            let file_path = string_field(item, &["path"])
                .or_else(|| string_field(item, &["name"]))
                .ok_or_else(|| SearchError::ParseError("missing github file path".to_string()))?;

            let repo = string_field(item, &["repository", "full_name"])
                .or_else(|| string_field(item, &["repository", "name"]))
                .unwrap_or_else(|| "unknown".to_string());

            let line_number = item
                .get("line_number")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0);

            let content = item
                .get("text_matches")
                .and_then(Value::as_array)
                .and_then(|matches| matches.first())
                .and_then(|first| first.get("fragment"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_default();

            let language = string_field(item, &["language"]);

            let url = string_field(item, &["html_url"])
                .or_else(|| string_field(item, &["url"]))
                .unwrap_or_default();

            parsed.push(CodeSearchResult {
                file_path,
                repo,
                line_number,
                content,
                language,
                url,
            });
        }

        Ok(parsed)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SearchError {
    #[error("network error: {0}")]
    NetworkError(String),
    #[error("parse error: {0}")]
    ParseError(String),
    #[error("rate limited, retry in {0}s")]
    RateLimited(u64),
    #[error("authentication required")]
    AuthenticationRequired,
    #[error("provider unavailable: {0}")]
    ProviderUnavailable(String),
}

fn string_field(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(ToString::to_string)
}

const fn default_max_results() -> usize {
    20
}

const fn default_timeout_secs() -> u64 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_query() -> CodeSearchQuery {
        CodeSearchQuery {
            query: "ResourceRegistry".to_string(),
            language: None,
            repo_filter: None,
            file_filter: None,
            max_results: None,
        }
    }

    #[test]
    fn config_default_values_match_spec() {
        let config = CodeSearchConfig::default();
        assert_eq!(config.max_results, 20);
        assert_eq!(config.timeout_secs, 30);
        assert!(config.language_filter.is_none());
    }

    #[test]
    fn config_deserialize_uses_field_defaults() {
        let decoded: CodeSearchConfig = serde_json::from_str(
            r#"{"provider":{"GitHubCode":{"token":null}},"language_filter":"Rust"}"#,
        )
        .expect("config should deserialize with defaults");

        assert_eq!(decoded.max_results, 20);
        assert_eq!(decoded.timeout_secs, 30);
        assert_eq!(decoded.language_filter.as_deref(), Some("Rust"));
    }

    #[test]
    fn build_sourcegraph_query_applies_all_filters() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let query = CodeSearchQuery {
            query: "fn new".to_string(),
            language: Some("Rust".to_string()),
            repo_filter: Some("0xMugen/Othala".to_string()),
            file_filter: Some("crates/orchd".to_string()),
            max_results: Some(15),
        };

        let built = client.build_sourcegraph_query(&query);
        assert!(built.contains("fn new"));
        assert!(built.contains("repo:0xMugen/Othala"));
        assert!(built.contains("file:crates/orchd"));
        assert!(built.contains("lang:Rust"));
        assert!(built.contains("count:15"));
    }

    #[test]
    fn build_sourcegraph_query_uses_config_defaults() {
        let config = CodeSearchConfig {
            provider: SearchProvider::Sourcegraph {
                endpoint: "https://sg.test".to_string(),
            },
            max_results: 9,
            timeout_secs: 12,
            language_filter: Some("Go".to_string()),
        };
        let client = CodeSearchClient::new(config);
        let built = client.build_sourcegraph_query(&base_query());

        assert!(built.contains("lang:Go"));
        assert!(built.contains("count:9"));
    }

    #[test]
    fn build_github_query_applies_filters() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let query = CodeSearchQuery {
            query: "PromptRunner".to_string(),
            language: Some("Rust".to_string()),
            repo_filter: Some("org/repo".to_string()),
            file_filter: Some("src".to_string()),
            max_results: None,
        };

        let built = client.build_github_query(&query);
        assert!(built.contains("PromptRunner"));
        assert!(built.contains("repo:org/repo"));
        assert!(built.contains("path:src"));
        assert!(built.contains("language:Rust"));
    }

    #[test]
    fn parse_sourcegraph_response_extracts_results() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let payload = r#"{
            "data": {
                "search": {
                    "results": {
                        "results": [
                            {
                                "file": { "path": "src/lib.rs" },
                                "repository": { "name": "org/repo" },
                                "lineNumber": 42,
                                "content": "pub fn new() {}",
                                "language": "Rust",
                                "url": "https://example/result"
                            }
                        ]
                    }
                }
            }
        }"#;

        let parsed = client
            .parse_sourcegraph_response(payload)
            .expect("sourcegraph response should parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].file_path, "src/lib.rs");
        assert_eq!(parsed[0].repo, "org/repo");
        assert_eq!(parsed[0].line_number, 42);
        assert_eq!(parsed[0].language.as_deref(), Some("Rust"));
    }

    #[test]
    fn parse_sourcegraph_response_accepts_fallback_fields() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let payload = r#"{
            "data": {
                "search": {
                    "results": {
                        "results": [
                            {
                                "path": "src/main.rs",
                                "repo": "org/alt",
                                "preview": "fn main(){}"
                            }
                        ]
                    }
                }
            }
        }"#;

        let parsed = client
            .parse_sourcegraph_response(payload)
            .expect("fallback shape should parse");
        assert_eq!(parsed[0].file_path, "src/main.rs");
        assert_eq!(parsed[0].repo, "org/alt");
        assert_eq!(parsed[0].content, "fn main(){}".to_string());
        assert_eq!(parsed[0].line_number, 0);
    }

    #[test]
    fn parse_sourcegraph_response_reports_rate_limit() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let payload = r#"{
            "errors": [
                {
                    "message": "rate limited",
                    "extensions": { "retryAfterSeconds": 90 }
                }
            ]
        }"#;

        let err = client
            .parse_sourcegraph_response(payload)
            .expect_err("rate limit should return an error");
        assert_eq!(err, SearchError::RateLimited(90));
    }

    #[test]
    fn parse_sourcegraph_response_requires_results_array() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let err = client
            .parse_sourcegraph_response(r#"{"data":{}}"#)
            .expect_err("missing results must fail");
        assert!(matches!(err, SearchError::ParseError(_)));
    }

    #[test]
    fn parse_github_response_extracts_items() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let payload = r#"{
            "items": [
                {
                    "path": "src/lib.rs",
                    "repository": { "full_name": "org/repo" },
                    "line_number": 7,
                    "text_matches": [
                        { "fragment": "pub struct X" }
                    ],
                    "language": "Rust",
                    "html_url": "https://github.com/org/repo/blob/main/src/lib.rs"
                }
            ]
        }"#;

        let parsed = client
            .parse_github_response(payload)
            .expect("github response should parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].repo, "org/repo");
        assert_eq!(parsed[0].file_path, "src/lib.rs");
        assert_eq!(parsed[0].line_number, 7);
        assert_eq!(parsed[0].content, "pub struct X");
    }

    #[test]
    fn parse_github_response_uses_fallback_fields() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let payload = r#"{
            "items": [
                {
                    "name": "main.rs",
                    "repository": { "name": "repo-only" },
                    "url": "https://api.github.com/code"
                }
            ]
        }"#;

        let parsed = client
            .parse_github_response(payload)
            .expect("fallback item shape should parse");
        assert_eq!(parsed[0].file_path, "main.rs");
        assert_eq!(parsed[0].repo, "repo-only");
        assert_eq!(parsed[0].url, "https://api.github.com/code");
    }

    #[test]
    fn parse_github_response_requires_authentication() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let err = client
            .parse_github_response(r#"{"message":"Requires authentication"}"#)
            .expect_err("auth error should be returned");
        assert_eq!(err, SearchError::AuthenticationRequired);
    }

    #[test]
    fn parse_github_response_reports_rate_limit() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let err = client
            .parse_github_response(r#"{"rate_limit_reset":120}"#)
            .expect_err("rate limit should be reported");
        assert_eq!(err, SearchError::RateLimited(120));
    }

    #[test]
    fn parse_github_response_requires_items_array() {
        let client = CodeSearchClient::new(CodeSearchConfig::default());
        let err = client
            .parse_github_response(r#"{"total_count":0}"#)
            .expect_err("missing items must fail");
        assert!(matches!(err, SearchError::ParseError(_)));
    }

    #[test]
    fn search_error_display_is_readable() {
        assert_eq!(
            SearchError::ProviderUnavailable("down".to_string()).to_string(),
            "provider unavailable: down"
        );
        assert_eq!(SearchError::RateLimited(33).to_string(), "rate limited, retry in 33s");
    }
}
