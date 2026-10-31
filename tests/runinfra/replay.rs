//! Adapted from Maki's fixed provider replay harness; see THIRD_PARTY.md.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use maki_providers::model::ModelInfo;
use maki_providers::provider::Provider;
use maki_providers::test_support::{Recorded, Requests};
use maki_providers::{
    AgentError, Message, ProviderEvent, RequestOptions, StreamResponse, ThinkingSupport,
};
use maki_storage::id::SessionRef;
use maki_storage::sessions::StoredThinking;
use serde::Deserialize;
use serde_json::{Value, json};
use test_case::test_case;

use super::{
    Canned, KEY_ENV, Model, Runtime, SLUG, Timeouts, base_url_env_var, isolated_state,
    model_registry, plugin, runtime, serve,
};

const API_KEY: &str = "sk-replay";
const PROMPT: &str = "read a.txt";
const SYSTEM: &str = "You are a replay fixture.";
const TOOL_NAME: &str = "read";
const TOOL_DESCRIPTION: &str = "Read a file";
const VOLATILE_VALUE: &str = "<volatile>";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Golden {
    call: Call,
    script: Vec<Answer>,
    expected: Value,
}

#[derive(Deserialize)]
#[serde(tag = "endpoint", rename_all = "snake_case", deny_unknown_fields)]
enum Call {
    Models {},
    Discovered(Turn),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    model: String,
    thinking_override: Option<ThinkingSupport>,
    thinking: StoredThinking,
    session: Option<SessionRef>,
    messages: Option<Vec<Message>>,
    tools: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    path: Option<String>,
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

impl Answer {
    fn leak(self) -> Canned {
        let headers: Vec<(&'static str, &'static str)> = self
            .headers
            .into_iter()
            .map(|(name, value)| (&*name.leak(), &*value.leak()))
            .collect();
        Canned {
            status: self.status,
            headers: headers.leak(),
            body: self.body.leak(),
            path: self.path.map(|path| &*path.leak()),
        }
    }
}

#[test_case("models" ; "published_model_metadata")]
#[test_case("models_missing_and_invalid_metadata" ; "absent_or_invalid_metadata")]
#[test_case("models_unauthorized" ; "authentication_error")]
#[test_case("max_snaps_to_xhigh" ; "published_effort_ceiling")]
#[test_case("off_only_leaves_default_effort" ; "off_only_preserves_default")]
#[test_case("off_only_sends_none" ; "off_only_sends_none")]
#[test_case("off_with_none" ; "off_with_none")]
#[test_case("off_without_none" ; "off_without_none")]
#[test_case("undeclared_efforts_leave_default" ; "undeclared_efforts_preserve_default")]
fn fixed_golden_replays(case: &str) {
    let _home = isolated_state();
    let package = PathBuf::from(env::var("MAKI_ESSENTIAL_PACKAGE").unwrap());
    let path = package
        .join("tests/goldens/runinfra")
        .join(format!("{case}.json"));
    let golden: Golden = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let script: Vec<Canned> = golden.script.into_iter().map(Answer::leak).collect();
    let (base_url, requests) = serve(script.leak());
    unsafe {
        env::set_var(base_url_env_var(SLUG), base_url);
        env::set_var(KEY_ENV, API_KEY);
    }
    let _runtime = Runtime::with_provider_grants(&[SLUG]);
    let provider = plugin::create(SLUG, Timeouts::default()).unwrap();
    let observed = match golden.call {
        Call::Models {} => json!({
            "outcome": listing(&runtime().block_on(provider.list_models())),
            "requests": recorded(&requests),
        }),
        Call::Discovered(turn) => {
            let mut model = Model::from_spec(&turn.model).unwrap();
            if let Some(support) = turn.thinking_override {
                model.thinking_override = Some(support);
            }
            let models = runtime().block_on(provider.list_models());
            if let Ok(models) = &models {
                model_registry::set_known_models(SLUG, models.clone());
            }
            let (events, result) = exchange(provider.as_ref(), &model, turn);
            json!({
                "discovery": listing(&models),
                "requests": recorded(&requests),
                "events": events,
                "outcome": match &result {
                    Ok(response) => json!({
                        "message": response.message,
                        "usage": response.usage,
                        "stop_reason": response.stop_reason,
                        "context_size": response.usage.total_input(),
                    }),
                    Err(error) => failure(error),
                },
            })
        }
    };
    assert_eq!(
        canonical(&observed),
        canonical(&golden.expected),
        "{}",
        path.display()
    );
}

fn tools() -> Value {
    json!([{
        "name": TOOL_NAME,
        "description": TOOL_DESCRIPTION,
        "input_schema": {
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
        },
    }])
}

fn exchange(
    provider: &dyn Provider,
    model: &Model,
    turn: Turn,
) -> (Vec<ProviderEvent>, Result<StreamResponse, AgentError>) {
    let messages = turn
        .messages
        .unwrap_or_else(|| vec![Message::user(PROMPT.to_owned())]);
    let tools = turn.tools.unwrap_or_else(tools);
    let (tx, rx) = flume::unbounded();
    let result = runtime().block_on(provider.stream_message(
        model,
        &messages,
        SYSTEM,
        &tools,
        &tx,
        RequestOptions {
            thinking: turn.thinking.into(),
            fast: false,
        },
        turn.session.as_ref(),
    ));
    drop(tx);
    (rx.drain().collect(), result)
}

fn listing(models: &Result<Vec<ModelInfo>, AgentError>) -> Value {
    match models {
        Ok(models) => json!({"models": models}),
        Err(error) => failure(error),
    }
}

fn failure(error: &AgentError) -> Value {
    json!({"error": format!("{:?}", error.projection()), "message": error.to_string()})
}

fn recorded(requests: &Requests) -> Value {
    Value::Array(requests.lock().unwrap().iter().map(request_value).collect())
}

fn request_value(request: &Recorded) -> Value {
    let headers: BTreeMap<&str, &str> = request
        .headers
        .iter()
        .map(|(name, value)| {
            let value = match name.as_str() {
                "host" | "user-agent" => VOLATILE_VALUE,
                _ => value.as_str(),
            };
            (name.as_str(), value)
        })
        .collect();
    json!({
        "method": request.method,
        "path": request.path,
        "body": String::from_utf8_lossy(&request.body),
        "headers": headers,
    })
}

fn canonical(value: &Value) -> Value {
    let mut value: Value = serde_json::from_str(&value.to_string()).unwrap();
    for request in value["requests"].as_array_mut().unwrap() {
        if let Some(body) = request["body"].as_str()
            && let Ok(parsed) = serde_json::from_str::<Value>(body)
        {
            request["body"] = Value::String(sorted(&parsed).to_string());
        }
    }
    sorted(&value)
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), sorted(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(sorted).collect()),
        scalar => scalar.clone(),
    }
}
