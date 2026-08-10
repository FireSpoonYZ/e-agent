use e_agent_tool::{Context, Result};
use tavily::{SearchResponse, Tavily};

pub(crate) async fn attraction_inner(city: String, weather: String) -> Result<SearchResponse> {
    let api_key = std::env::var("TAVILY_API_KEY").context("TAVILY_API_KEY is not set")?;
    let client = Tavily::builder(api_key)
        .build()
        .context("create Tavily client failed")?;
    let query = format!(
        "'{}' 在'{}'天气下最值得去的旅游景点推荐及理由",
        city, weather
    );
    let resp = client.answer(query).await.context("查询失败")?;
    Ok(resp)
}
