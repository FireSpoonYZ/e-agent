use e_agent_extension::{Context, JsonSchema, Result, Serialize};
use serde::Deserialize;
use websearch::{SearchOptions, SearchProvider, providers::SearxNGProvider};

/// Fetch web search results through the websearch SearXNG provider.
pub async fn run(
    query: String,
    max_results: Option<u32>,
    language: Option<String>,
    region: Option<String>,
    page: Option<u32>,
    timeout: Option<u64>,
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
