use e_agent_tool::{Context, JsonSchema, Result, Serialize, tool};
use serde::Deserialize;
use websearch::{SearchOptions, SearchProvider, providers::SearxNGProvider};

#[tool]
/// 异步查询指定城市的实时天气
pub async fn web_fetch(
    #[desc("The search query text")] query: String,
    #[desc("Maximum number of results to return")] max_results: Option<u32>,
    #[desc("Language/locale for results")] language: Option<String>,
    #[desc("Country/region for results")] region: Option<String>,
    #[desc("Result page number (for pagination)")] page: Option<u32>,
    #[desc("Custom timeout in milliseconds")] timeout: Option<u64>,
) -> Result<Vec<SearchResult>> {
    let base_url = std::env::var("SEARXNG_BASE_URL").context("SEARXNG_BASE_URL is not set")?;
    let provider = SearxNGProvider::new(&base_url).context("connect to searxng failed")?;
    let results = provider
        .search(&SearchOptions {
            query,
            max_results,
            language,
            region,
            page,
            timeout,
            ..Default::default()
        })
        .await
        .context("search failed")?
        .into_iter()
        .map(|result| SearchResult::from(result))
        .collect();
    Ok(results)
}

/// Represents a web search result returned by any search provider
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    /// URL of the search result
    pub url: String,
    /// Title of the web page
    pub title: String,
    /// Snippet/description of the web page
    pub snippet: Option<String>,
    /// The source website domain
    pub domain: Option<String>,
    /// When the result was published or last updated
    pub published_date: Option<String>,
    /// The search provider that returned this result
    pub provider: Option<String>,
    /// Raw response data from the provider
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl From<websearch::SearchResult> for SearchResult {
    fn from(value: websearch::SearchResult) -> Self {
        Self {
            url: value.url,
            title: value.title,
            snippet: value.snippet,
            domain: value.domain,
            published_date: value.published_date,
            provider: value.provider,
            raw: value.raw,
        }
    }
}
