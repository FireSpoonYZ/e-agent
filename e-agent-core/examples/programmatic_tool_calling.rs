use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY").context("OPENAI_API_KEY is not set")?;
    let base_url = std::env::var("OPENAI_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    let module = std::env::var("E_MODULE_BIG").context("E_MODULE_BIG is not set")?;
    let (model, effort) = module
        .split_once(':')
        .context("E_MODULE_BIG must use model:reasoning_effort")?;

    let tools = json!([
        {
            "type": "function",
            "name": "get_inventory",
            "description": "Return inventory for a SKU.",
            "parameters": {
                "type": "object",
                "properties": { "sku": { "type": "string" } },
                "required": ["sku"],
                "additionalProperties": false
            },
            "output_schema": {
                "type": "object",
                "properties": {
                    "sku": { "type": "string" },
                    "available_units": { "type": "number" }
                },
                "required": ["sku", "available_units"],
                "additionalProperties": false
            },
            "allowed_callers": ["programmatic"]
        },
        {
            "type": "function",
            "name": "get_demand",
            "description": "Return demand for a SKU.",
            "parameters": {
                "type": "object",
                "properties": { "sku": { "type": "string" } },
                "required": ["sku"],
                "additionalProperties": false
            },
            "output_schema": {
                "type": "object",
                "properties": {
                    "sku": { "type": "string" },
                    "requested_units": { "type": "number" }
                },
                "required": ["sku", "requested_units"],
                "additionalProperties": false
            },
            "allowed_callers": ["programmatic"]
        },
        { "type": "programmatic_tool_calling" }
    ]);
    let mut input = vec![json!({
        "role": "user",
        "content": "Use Programmatic Tool Calling to compare inventory and demand for sku_123. Return the shortage."
    })];
    let client = reqwest::Client::builder()
        .user_agent("e-agent/programmatic-tool-calling-smoke-test")
        .build()?;

    for round in 1..=10 {
        let response = client
            .post(format!("{}/responses", base_url.trim_end_matches('/')))
            .bearer_auth(&api_key)
            .json(&json!({
                "model": model,
                "reasoning": { "effort": effort },
                "store": false,
                "input": input,
                "tools": tools
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            bail!("POST /responses returned {status}: {body}");
        }

        let response: Value = serde_json::from_str(&body).context("invalid Responses JSON")?;
        let output = response["output"]
            .as_array()
            .context("response.output is not an array")?;
        input.extend(output.iter().cloned());

        let mut call_count = 0;
        for call in output.iter().filter(|item| item["type"] == "function_call") {
            call_count += 1;
            let name = call["name"].as_str().context("function call has no name")?;
            let arguments: Value = serde_json::from_str(
                call["arguments"]
                    .as_str()
                    .context("function call arguments are not a string")?,
            )?;
            let sku = arguments["sku"].as_str().context("sku is missing")?;
            let result = match name {
                "get_inventory" => json!({ "sku": sku, "available_units": 42 }),
                "get_demand" => json!({ "sku": sku, "requested_units": 51 }),
                _ => bail!("unknown local tool: {name}"),
            };
            input.push(json!({
                "type": "function_call_output",
                "call_id": call["call_id"],
                "output": serde_json::to_string(&result)?,
                "caller": call["caller"]
            }));
            println!("round {round}: executed {name} locally -> {result}");
        }

        if call_count == 0
            && let Some(text) = response["output_text"].as_str()
            && !text.is_empty()
        {
            println!("final: {text}");
            return Ok(());
        }
    }

    bail!("no final message after 10 rounds")
}
