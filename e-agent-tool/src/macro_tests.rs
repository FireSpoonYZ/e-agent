//! Macro-level tests for `#[extension]`, `#[state]`, and `#[tool]`.

use serde_json::json;

use crate::{
    SessionId, Tool, ToolExtension, clear_current_session, extension, set_current_session,
    tool_function,
};

#[extension(
    description = "Remember values per session",
    system_prompt = "Use notes to remember values."
)]
mod notes {
    use crate::Result;

    #[state]
    #[derive(Default)]
    pub struct Remembered {
        values: Vec<String>,
    }

    #[tool]
    /// Remember one value and return the whole list.
    pub async fn remember(
        #[state] state: &mut Remembered,
        #[desc("Value to remember")] value: String,
        #[desc("Optional label stored with the value")] label: Option<String>,
    ) -> Result<String> {
        match label {
            Some(label) => state.values.push(format!("{label}:{value}")),
            None => state.values.push(value),
        }
        Ok(state.values.join(","))
    }

    #[tool]
    /// List every remembered value.
    pub async fn recall(#[state] state: &Remembered) -> Result<String> {
        Ok(state.values.join(","))
    }
}

#[extension(description = "A second stateful extension")]
mod other_notes {
    use crate::Result;

    #[state]
    #[derive(Default)]
    pub struct Remembered {
        values: Vec<String>,
    }

    #[tool]
    /// Remember one value in this extension only.
    pub async fn remember(
        #[state] state: &mut Remembered,
        #[desc("Value to remember")] value: String,
    ) -> Result<String> {
        state.values.push(value);
        Ok(state.values.join(","))
    }
}

#[extension(description = "A stateless extension")]
mod plain {
    use crate::Result;

    #[tool]
    /// Double a number.
    pub async fn double(#[desc("Number to double")] value: i64) -> Result<i64> {
        Ok(value * 2)
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn call<T: Tool>(input: serde_json::Value) -> String
where
    T::Input: serde::de::DeserializeOwned,
    T::Output: std::fmt::Debug,
{
    let input: T::Input = serde_json::from_value(input).unwrap();
    format!("{:?}", T::call(input).await.unwrap())
}

/// The exported metadata must carry name, description, prompt, and every tool.
#[test]
fn exports_extension_metadata() {
    let functions = vec![
        tool_function::<notes::remember::Definition>().unwrap(),
        tool_function::<notes::recall::Definition>().unwrap(),
    ];
    let extension = ToolExtension {
        name: "notes".to_string(),
        description: "Remember values per session".to_string(),
        system_prompt: "Use notes to remember values.".to_string(),
        functions,
    };

    assert_eq!(
        extension.functions[0].description,
        "Remember one value and return the whole list."
    );
    assert!(extension.functions.iter().all(|tool| tool.requires_await));
    assert_eq!(
        extension.functions[0].schema["properties"]["value"]["description"],
        "Value to remember"
    );
    assert_eq!(
        extension.functions[0].schema["properties"]["label"]["description"],
        "Optional label stored with the value"
    );
    // Optional parameters stay out of `required`.
    assert_eq!(extension.functions[0].schema["required"], json!(["value"]));
    assert_eq!(extension.functions[1].name, "recall");

    // An omitted system_prompt defaults to an empty string.
    let round_trip: ToolExtension = serde_json::from_str(
        &serde_json::to_string(&ToolExtension {
            name: "other_notes".to_string(),
            description: "A second stateful extension".to_string(),
            system_prompt: String::new(),
            functions: vec![tool_function::<other_notes::remember::Definition>().unwrap()],
        })
        .unwrap(),
    )
    .unwrap();
    assert!(round_trip.system_prompt.is_empty());
    assert_eq!(round_trip.functions[0].name, "remember");

    let mut metadata = serde_json::to_value(&round_trip).unwrap();
    metadata["functions"][0]
        .as_object_mut()
        .unwrap()
        .remove("requires_await");
    assert!(serde_json::from_value::<ToolExtension>(metadata).is_err());
}

/// A `#[state]` parameter must not reach the input schema or Python signature.
#[test]
fn hides_state_from_schema() {
    let remember = tool_function::<notes::remember::Definition>().unwrap();
    let properties = remember.schema["properties"].as_object().unwrap();
    assert!(!properties.contains_key("state"));
    assert_eq!(
        properties.keys().collect::<Vec<_>>(),
        vec!["label", "value"]
    );

    // A state-only tool exposes no model-visible parameters at all.
    let recall = tool_function::<notes::recall::Definition>().unwrap();
    assert_eq!(
        recall.schema["properties"]
            .as_object()
            .map_or(0, |properties| properties.len()),
        0
    );
}

/// Tools in one extension and session share one state object.
#[test]
fn shares_state_within_one_session() {
    let _guard = crate::state::test_guard();
    runtime().block_on(async {
        set_current_session(SessionId::next());
        assert_eq!(
            call::<notes::remember::Definition>(json!({"value": "a", "label": null})).await,
            "\"a\""
        );
        call::<notes::remember::Definition>(json!({"value": "b", "label": "x"})).await;
        assert_eq!(
            call::<notes::recall::Definition>(json!({})).await,
            "\"a,x:b\""
        );
        clear_current_session();
    });
}

/// Different sessions get different state objects, and dropping one keeps the other.
#[test]
fn isolates_and_drops_sessions() {
    let _guard = crate::state::test_guard();
    runtime().block_on(async {
        let first = SessionId::next();
        let second = SessionId::next();

        set_current_session(first);
        call::<notes::remember::Definition>(json!({"value": "first", "label": null})).await;
        set_current_session(second);
        assert_eq!(call::<notes::recall::Definition>(json!({})).await, "\"\"");
        call::<notes::remember::Definition>(json!({"value": "second", "label": null})).await;

        set_current_session(first);
        assert_eq!(
            call::<notes::recall::Definition>(json!({})).await,
            "\"first\""
        );

        notes::__E_AGENT_STATES.drop_session(first);
        assert_eq!(call::<notes::recall::Definition>(json!({})).await, "\"\"");
        set_current_session(second);
        assert_eq!(
            call::<notes::recall::Definition>(json!({})).await,
            "\"second\""
        );
        clear_current_session();
    });
}

/// Two stateful extensions own separate state maps.
#[test]
fn isolates_extensions() {
    let _guard = crate::state::test_guard();
    runtime().block_on(async {
        set_current_session(SessionId::next());
        call::<notes::remember::Definition>(json!({"value": "notes", "label": null})).await;
        assert_eq!(
            call::<other_notes::remember::Definition>(json!({"value": "other"})).await,
            "\"other\""
        );
        assert_eq!(
            call::<notes::recall::Definition>(json!({})).await,
            "\"notes\""
        );
        clear_current_session();
    });
}

/// Stateless tools keep working without any state struct or parameter.
#[test]
fn supports_stateless_tools() {
    runtime().block_on(async {
        assert_eq!(
            call::<plain::double::Definition>(json!({"value": 21})).await,
            "42"
        );
    });
    let double = tool_function::<plain::double::Definition>().unwrap();
    assert_eq!(double.schema["required"], json!(["value"]));
}
