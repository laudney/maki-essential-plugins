use std::{env, fs};

use maki_config::providers::base_url_env_var;
use maki_providers::model_registry;
use maki_providers::plugin;
use maki_providers::provider::{available_model_specs, from_model};
use maki_providers::test_support::{Canned, serve};
use maki_providers::{Message, Model, RequestOptions, Timeouts, catalog_provider, warm_catalog};
use maki_storage::StateDir;
use maki_storage::auth::{ProviderCredentials, save_provider_credentials};
use serde_json::{Value, json};
use tempfile::TempDir;
use test_case::test_case;

use super::{PACKAGE, Runtime};

const SLUG: &str = "fireworks-ai";
const KEY_ENV: &str = "FIREWORKS_API_KEY";
const API_KEY: &str = "sk-fireworks-test";
const MODEL_ID: &str = "accounts/fireworks/models/glm-5p3-flash";
const MODEL: &str = "fireworks-ai/accounts/fireworks/models/glm-5p3-flash";
const OBSOLETE: &str = "accounts/fireworks/models/obsolete-catalog-id";
const MODELS_PATH: &str =
    "/v1/accounts/fireworks/models?filter=supports_serverless%3Dtrue&pageSize=200";
const PAGE_1: &str = include_str!("fixtures/fireworks/models_page_1.json");
const PAGE_2: &str = include_str!("fixtures/fireworks/models_page_2.json");
const EMPTY_PAGE: &str = r#"{"models":[]}"#;

fn isolated_state(selected: bool) -> TempDir {
    let home = TempDir::new().unwrap();
    for name in [
        "HOME",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
    ] {
        unsafe { env::set_var(name, home.path()) };
    }
    for name in [KEY_ENV, &base_url_env_var(SLUG)] {
        unsafe { env::remove_var(name) };
    }
    let config = maki_storage::paths::config_dir().unwrap();
    fs::create_dir_all(&config).unwrap();
    if selected {
        fs::write(
            config.join("providers.toml"),
            format!("[{SLUG}]\nplugin = \"{PACKAGE}\"\n"),
        )
        .unwrap();
    }
    home
}

#[test]
fn unselected_package_keeps_existing_plugins_without_fireworks() {
    let _home = isolated_state(false);
    let runtime = Runtime::new();
    assert!(runtime.registry.get("get_goal").is_some());
    assert!(runtime.registry.get("monitor").is_some());
    assert!(!plugin::is_registered(SLUG));
}

#[test]
fn saved_key_and_live_paginated_models_replace_stale_catalog() {
    let _home = isolated_state(true);
    let cache = maki_storage::paths::cache_dir().unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::write(
        cache.join("models-dev-catalog.json"),
        json!({SLUG: {
            "name": "Fireworks AI",
            "npm": "@ai-sdk/openai-compatible",
            "api": "https://api.fireworks.ai/inference/v1",
            "env": [KEY_ENV],
            "models": {
                MODEL_ID: {"limit": {"context": 4096, "output": 128}, "modalities": {"input": ["text", "image"]}},
                OBSOLETE: {"limit": {"context": 4096}},
            },
        }})
        .to_string(),
    )
    .unwrap();
    warm_catalog();
    assert!(
        catalog_provider(SLUG)
            .unwrap()
            .models
            .contains_key(OBSOLETE)
    );
    save_provider_credentials(
        &StateDir::resolve().unwrap(),
        SLUG,
        &ProviderCredentials {
            api_key: API_KEY.into(),
            host: None,
        },
    )
    .unwrap();
    let (base_url, requests) = serve(Box::leak(Box::new([
        Canned::json(200, PAGE_1),
        Canned::json(200, PAGE_2),
    ])));
    unsafe {
        env::set_var(
            base_url_env_var(SLUG),
            base_url.replace("/v1", "/inference/v1/"),
        )
    };
    let _runtime = Runtime::new();
    let mut model = Model::from_spec(MODEL).unwrap();
    let provider = from_model(&mut model, Timeouts::default()).unwrap();
    let models = smol::block_on(provider.list_models()).unwrap();
    assert_eq!(
        models.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        [
            MODEL_ID,
            "accounts/fireworks/models/kimi-k3",
            "accounts/fireworks/models/new-chat"
        ]
    );
    let glm = &models[0];
    assert_eq!(glm.context_window, Some(1048576));
    assert_eq!(glm.supports_vision, Some(false));
    assert!(glm.max_output_tokens.is_none());
    assert!(glm.pricing.is_none());
    assert!(glm.supports_thinking.is_none());
    assert_eq!(models[1].supports_vision, Some(true));
    assert!(models[2].context_window.is_none());
    model_registry::set_known_models(SLUG, models);
    let model = Model::from_spec(MODEL).unwrap();
    assert_eq!(model.context_window, 1048576);
    // Unpublished fields keep Maki's normal catalog fallback.
    assert_eq!(model.max_output_tokens, Some(128));
    assert!(!model.supports_vision());
    let policy = maki_config::ModelPolicy::new(&[format!("{SLUG}/*")], &[]).unwrap();
    assert!(!available_model_specs(&policy).contains(&format!("{SLUG}/{OBSOLETE}")));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, MODELS_PATH);
    assert_eq!(
        requests[1].path,
        format!("{MODELS_PATH}&pageToken=page%2B%2F%3D2%26x")
    );
    for request in requests.iter() {
        assert_eq!(request.authorization(), format!("Bearer {API_KEY}"));
    }
}

#[test_case("/inference/v1", "/v1" ; "standard_api_paths")]
#[test_case("/proxy/inference/v1", "/proxy/v1" ; "gateway_prefix")]
#[test_case("/v1", "/v1" ; "compatible_gateway")]
fn environment_key_and_base_url_control_discovery_and_chat(inference: &str, management: &str) {
    let _home = isolated_state(true);
    let (base_url, requests) = serve(Box::leak(Box::new([
        Canned::json(200, EMPTY_PAGE),
        Canned::sse(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        ),
    ])));
    let origin = base_url.strip_suffix("/v1").unwrap();
    unsafe {
        env::set_var(KEY_ENV, API_KEY);
        env::set_var(base_url_env_var(SLUG), format!("{origin}{inference}"));
    }
    let _runtime = Runtime::new();
    let mut model = Model::from_spec(MODEL).unwrap();
    let provider = from_model(&mut model, Timeouts::default()).unwrap();
    assert!(smol::block_on(provider.list_models()).unwrap().is_empty());
    let (tx, _rx) = flume::unbounded();
    let response = smol::block_on(provider.stream_message(
        &model,
        &[Message::user("Hello".into())],
        "Test",
        &json!([]),
        &tx,
        RequestOptions::default(),
        None,
    ))
    .unwrap();
    assert!(!response.message.content.is_empty());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].path,
        format!(
            "{management}/accounts/fireworks/models?filter=supports_serverless%3Dtrue&pageSize=200"
        )
    );
    assert_eq!(
        requests[1].path,
        format!("{}/chat/completions", inference.trim_end_matches('/'))
    );
    assert_eq!(requests[1].method, "POST");
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["model"], MODEL_ID);
    assert!(body.get("max_tokens").is_none());
    for request in requests.iter() {
        assert_eq!(request.authorization(), format!("Bearer {API_KEY}"));
    }
}

#[test_case(Value::Null ; "missing")]
#[test_case(json!(0) ; "zero")]
#[test_case(json!(-1) ; "negative")]
#[test_case(json!(1.5) ; "fractional")]
#[test_case(json!(4294967296_u64) ; "overflow")]
#[test_case(json!("8192") ; "string")]
fn invalid_context_stays_unspecified(context: Value) {
    let _home = isolated_state(true);
    let body = json!({"models": [{
        "name": MODEL_ID,
        "supportsServerless": true,
        "conversationConfig": {},
        "contextLength": context,
    }]})
    .to_string();
    let (base_url, _) = serve(Box::leak(Box::new([Canned::json(200, body.leak())])));
    unsafe {
        env::set_var(KEY_ENV, API_KEY);
        env::set_var(base_url_env_var(SLUG), base_url);
    }
    let _runtime = Runtime::new();
    let provider = plugin::create(SLUG, Timeouts::default()).unwrap();
    let models = smol::block_on(provider.list_models()).unwrap();
    assert_eq!(models.len(), 1);
    assert!(models[0].context_window.is_none());
}

#[test_case(401, r#"{"error":{"message":"invalid key"}}"#, "invalid key" ; "unauthorized")]
#[test_case(200, r#"{"models":"invalid"}"#, "Invalid Fireworks model listing" ; "invalid_models")]
#[test_case(200, r#"{"models":[],"nextPageToken":123}"#, "Invalid Fireworks model page token" ; "invalid_token")]
#[test_case(200, r#"{"models":[],"nextPageToken":"page+/=2&x"}"#, "Invalid Fireworks model page token" ; "repeated_token")]
fn page_failure_returns_error_instead_of_partial_models(
    status: u16,
    body: &'static str,
    error: &str,
) {
    let _home = isolated_state(true);
    let script = [Canned::json(200, PAGE_1), Canned::json(status, body)];
    let (base_url, requests) = serve(Box::leak(Box::new(script)));
    unsafe {
        env::set_var(KEY_ENV, API_KEY);
        env::set_var(base_url_env_var(SLUG), base_url);
    }
    let _runtime = Runtime::new();
    let provider = plugin::create(SLUG, Timeouts::default()).unwrap();
    let failure = smol::block_on(provider.list_models()).unwrap_err();
    assert!(failure.to_string().contains(error), "{failure}");
    assert_eq!(requests.lock().unwrap().len(), 2);
}
