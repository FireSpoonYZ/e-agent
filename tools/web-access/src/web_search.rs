use e_agent_extension::{Context, JsonSchema, Result, Serialize};
use serde::Deserialize;

/// Search the web through the configured SearXNG instance.
pub async fn run(
    q: String,
    categories: Option<String>,
    language: Option<String>,
    pageno: Option<u32>,
    time_range: Option<String>,
    safesearch: Option<u8>,
    theme: Option<String>,
    accept_invalid_certs: Option<bool>,
) -> Result<SearxngSearchResponse> {
    let base_url = std::env::var("SEARXNG_BASE_URL").context("SEARXNG_BASE_URL is not set")?;
    let params = SearxngSearchParams {
        q,
        categories,
        language,
        pageno,
        time_range,
        safesearch,
        theme,
    };

    reqwest::ClientBuilder::new()
        .tls_danger_accept_invalid_certs(accept_invalid_certs.unwrap_or(true))
        .build()
        .context("reqwest client build failed")?
        .get(format!("{}/search", base_url.trim_end_matches('/')))
        .query(&params)
        .query(&[("format", "json")])
        .send()
        .await
        .context("request SearXNG failed")?
        .error_for_status()
        .context("SearXNG returned an error status")?
        .json()
        .await
        .context("decode SearXNG response failed")
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearxngSearchParams {
    /// Search query. SearXNG search syntax, such as `site:github.com rust`, is supported.
    pub q: String,
    /// Comma-separated search categories enabled by the instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub categories: Option<String>,
    /// Result language code, for example `zh-CN` or `en`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// One-based result page number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pageno: Option<u32>,
    /// Supported values are `day`, `month`, and `year`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_range: Option<String>,
    /// Safe-search level: `0`, `1`, or `2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safesearch: Option<u8>,
    /// Theme configured by the SearXNG instance, normally `simple`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearxngSearchResponse {
    pub query: String,
    #[serde(default)]
    pub results: Vec<serde_json::Value>,
    #[serde(default)]
    pub answers: Vec<serde_json::Value>,
    #[serde(default)]
    pub corrections: Vec<String>,
    #[serde(default)]
    pub infoboxes: Vec<serde_json::Value>,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub unresponsive_engines: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::SearxngSearchResponse;

    #[test]
    fn deserializes_official_response_shape() {
        let response: SearxngSearchResponse = serde_json::from_str(
            r#"{
                "query":"rust",
                "results":[{"title":"Rust","url":"https://www.rust-lang.org/"}],
                "answers":[],
                "corrections":[],
                "infoboxes":[],
                "suggestions":["rust language"],
                "unresponsive_engines":[["example","timeout"]]
            }"#,
        )
        .unwrap();

        assert_eq!(response.query, "rust");
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.unresponsive_engines[0].0, "example");
    }
}
