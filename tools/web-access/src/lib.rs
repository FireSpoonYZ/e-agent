#[path = "web_search.rs"]
mod web_search_impl;

use e_agent_extension::{Result, extension};
use web_search_impl::SearxngSearchResponse;

#[extension(
    description = "Search the web through the configured SearXNG instance",
    system_prompt = "Use web_access when a question needs current information from the web."
)]
mod web_access {
    use super::*;

    #[tool]
    /// Search the web through the configured SearXNG instance.
    async fn web_search(
        #[desc("Search query")] q: String,
        #[desc("Comma-separated search categories")] categories: Option<String>,
        #[desc("Result language code, such as zh-CN or en")] language: Option<String>,
        #[desc("One-based result page number")] pageno: Option<u32>,
        #[desc("Time range: day, month, or year")] time_range: Option<String>,
        #[desc("Safe-search level: 0, 1, or 2")] safesearch: Option<u8>,
        #[desc("Instance theme, normally simple")] theme: Option<String>,
        #[desc("Controls the use of certificate validation.")] accept_invalid_certs: Option<bool>,
    ) -> Result<SearxngSearchResponse> {
        web_search_impl::run(
            q,
            categories,
            language,
            pageno,
            time_range,
            safesearch,
            theme,
            accept_invalid_certs,
        )
        .await
    }
}
