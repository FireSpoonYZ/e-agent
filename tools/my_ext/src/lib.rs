mod attraction;
#[path = "weather.rs"]
mod weather_api;

use e_agent_extension::{Context, JsonSchema, Result, Serialize, extension};

#[derive(JsonSchema, Serialize)]
/// 指定城市的实时天气
struct WeatherOutput {
    /// 天气状况描述
    weather_desc: String,
    /// 摄氏温度
    temp_c: String,
}

#[extension(
    description = "查询城市实时天气并按天气推荐旅游景点",
    system_prompt = "需要天气或景点信息时，使用 my_ext 的工具，不要凭记忆回答。"
)]
mod my_ext {
    use super::*;

    #[tool]
    /// 异步查询指定城市的实时天气
    async fn weather(
        #[desc("需要查询实时天气的城市名称")] city: String
    ) -> Result<WeatherOutput> {
        let weather = weather_api::weather_inner(city).await?;
        Ok(WeatherOutput {
            weather_desc: weather.weather_desc,
            temp_c: weather.temp_c,
        })
    }

    #[tool]
    /// 异步搜索指定城市适合当前天气的旅游景点
    async fn get_attraction(
        #[desc("需要推荐旅游景点的城市名称")] city: String,
        #[desc("用于筛选合适景点的当前天气描述")] weather: String,
    ) -> Result<String> {
        attraction::attraction_inner(city, weather)
            .await?
            .answer
            .context("Tavily response has no answer")
    }
}
