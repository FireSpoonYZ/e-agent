use e_agent_extension::{Context, Deserialize, Result, Serialize, anyhow};

pub async fn weather_inner(city: String) -> Result<Weather> {
    let city = city.to_owned();
    let url = format!("https://wttr.in/{}?format=j1", city);
    let resp = reqwest::get(url)
        .await
        .context("request wttr failed")?
        .text()
        .await
        .context("wttr response body is incomplete")?;
    let mut resp = serde_json::from_str::<WeatherResponse>(&resp)
        .context("wttr response body deserialize failed")?;
    let mut current_condition = resp.current_condition.pop().ok_or_else(|| {
        anyhow!("wttr response body deserialize failed: current_condition is empty")
    })?;
    let weather_desc = current_condition
        .weather_desc
        .pop()
        .ok_or_else(|| {
            anyhow!(
                "wttr response body deserialize failed: current_condition.weather_desc is empty"
            )
        })?
        .value;
    let temp_c = current_condition.temp_c;

    Ok(Weather {
        weather_desc,
        temp_c,
    })
}

pub struct Weather {
    pub weather_desc: String,
    pub temp_c: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WeatherResponse {
    pub current_condition: Vec<CurrentCondition>,
    pub nearest_area: Vec<NearestArea>,
    pub request: Vec<WeatherRequest>,
    pub weather: Vec<DailyWeather>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CurrentCondition {
    #[serde(rename = "temp_C")]
    pub temp_c: String,
    #[serde(rename = "FeelsLikeC")]
    pub feels_like_c: String,
    pub humidity: String,
    pub cloudcover: String,
    #[serde(rename = "weatherDesc")]
    pub weather_desc: Vec<TextValue>,
    #[serde(rename = "weatherIconUrl")]
    pub weather_icon_url: Vec<TextValue>,
    #[serde(rename = "windspeedKmph")]
    pub windspeed_kmph: String,
    #[serde(rename = "winddir16Point")]
    pub wind_direction: String,
    #[serde(rename = "uvIndex")]
    pub uv_index: String,
    pub visibility: String,
    #[serde(rename = "observation_time")]
    pub observation_time: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NearestArea {
    #[serde(rename = "areaName")]
    pub area_name: Vec<TextValue>,
    pub country: Vec<TextValue>,
    pub region: Vec<TextValue>,
    pub latitude: String,
    pub longitude: String,
    pub population: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WeatherRequest {
    pub query: String,
    #[serde(rename = "type")]
    pub request_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DailyWeather {
    pub date: String,
    #[serde(rename = "avgtempC")]
    pub avg_temp_c: String,
    #[serde(rename = "maxtempC")]
    pub max_temp_c: String,
    #[serde(rename = "mintempC")]
    pub min_temp_c: String,
    #[serde(rename = "sunHour")]
    pub sun_hours: String,
    #[serde(rename = "uvIndex")]
    pub uv_index: String,
    pub astronomy: Vec<Astronomy>,
    pub hourly: Vec<HourlyWeather>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Astronomy {
    pub sunrise: String,
    pub sunset: String,
    pub moonrise: String,
    pub moonset: String,
    #[serde(rename = "moon_phase")]
    pub moon_phase: String,
    #[serde(rename = "moon_illumination")]
    pub moon_illumination: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HourlyWeather {
    pub time: String,
    #[serde(rename = "tempC")]
    pub temp_c: String,
    #[serde(rename = "FeelsLikeC")]
    pub feels_like_c: String,
    pub humidity: String,
    #[serde(rename = "chanceofrain")]
    pub chance_of_rain: String,
    #[serde(rename = "chanceofsnow")]
    pub chance_of_snow: String,
    #[serde(rename = "weatherDesc")]
    pub weather_desc: Vec<TextValue>,
    #[serde(rename = "windspeedKmph")]
    pub windspeed_kmph: String,
    #[serde(rename = "winddir16Point")]
    pub wind_direction: String,
    #[serde(rename = "uvIndex")]
    pub uv_index: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TextValue {
    pub value: String,
}
