use std::env;
use std::fs;

use maki_config::providers::base_url_env_var;
use maki_providers::model_registry;
use maki_providers::plugin;
use maki_providers::provider::{available_model_specs, from_model};
use maki_providers::test_support::{Canned, Requests, serve};
use maki_providers::{Model, ModelPricing, Timeouts, catalog_provider, warm_catalog};
use maki_storage::StateDir;
use maki_storage::auth::{ProviderCredentials, save_provider_credentials};
use serde_json::{Value, json};
use tempfile::TempDir;
use test_case::test_case;

use super::{PACKAGE, Runtime};

mod replay;

const SLUG: &str = "runinfra";
const KEY_ENV: &str = "RUNINFRA_GATEWAY_KEY";
const LEGACY_KEY_ENV: &str = "RUNINFRA_API_KEY";
const SAVED_KEY: &str = "sk-existing-runinfra";
const SAVED_AUTHORIZATION: &str = "Bearer sk-existing-runinfra";
const GATEWAY_KEY: &str = "sk-gateway-runinfra";
const LEGACY_KEY: &str = "sk-legacy-runinfra";
const EMPTY_KEY: &str = "   ";
const MODEL_ID: &str = "qwen3-8-27b";
const MODEL: &str = "runinfra/qwen3-8-27b";
const OBSOLETE_MODEL_ID: &str = "obsolete-catalog-id";
const CATALOG_CONTEXT: u32 = 4096;
const LIVE_CONTEXT: u32 = 262144;
const MODELS_GOLDEN: &str = include_str!("goldens/runinfra/models.json");
const MODELS_PATH: &str = "/v1/models";
const PROVIDERS_FILE: &str = "providers.toml";
const CATALOG_FILE: &str = "models-dev-catalog.json";
const HOME_VARS: &[&str] = &[
    "HOME",
    "XDG_STATE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
];

fn isolated_state(selected: bool) -> TempDir {
    let home = TempDir::new().unwrap();
    for name in HOME_VARS {
        unsafe { env::set_var(name, home.path()) };
    }
    for name in [KEY_ENV, LEGACY_KEY_ENV] {
        unsafe { env::remove_var(name) };
    }
    unsafe { env::remove_var(base_url_env_var(SLUG)) };
    let config = maki_storage::paths::config_dir().unwrap();
    fs::create_dir_all(&config).unwrap();
    if selected {
        fs::write(
            config.join(PROVIDERS_FILE),
            format!("[{SLUG}]\nplugin = \"{PACKAGE}\"\n"),
        )
        .unwrap();
    }
    home
}

fn stale_catalog() {
    let cache = maki_storage::paths::cache_dir().unwrap();
    fs::create_dir_all(&cache).unwrap();
    let catalog = json!({
        SLUG: {
            "name": "RunInfra",
            "npm": "@ai-sdk/openai-compatible",
            "api": "https://api.runinfra.ai/v1",
            "env": [KEY_ENV],
            "models": {
                MODEL_ID: {"limit": {"context": CATALOG_CONTEXT, "output": 128}},
                OBSOLETE_MODEL_ID: {"limit": {"context": CATALOG_CONTEXT}},
            },
        },
    });
    fs::write(cache.join(CATALOG_FILE), catalog.to_string()).unwrap();
}

fn model_server() -> (String, Requests) {
    let fixture: Value = serde_json::from_str(MODELS_GOLDEN).unwrap();
    let body = fixture["script"][0]["body"].as_str().unwrap().to_owned();
    serve(Box::leak(Box::new([Canned::json(200, body.leak())])))
}

#[test]
fn unselected_package_keeps_goal_and_monitor_without_runinfra() {
    let _home = isolated_state(false);
    stale_catalog();
    let runtime = Runtime::new();
    assert!(runtime.registry.get("get_goal").is_some());
    assert!(runtime.registry.get("monitor").is_some());
    assert!(!plugin::is_registered(SLUG));
}

#[test]
fn selected_package_uses_saved_key_and_live_metadata_over_cached_catalog() {
    let _home = isolated_state(true);
    stale_catalog();
    warm_catalog();
    let catalog = catalog_provider(SLUG).unwrap();
    assert!(catalog.models.contains_key(OBSOLETE_MODEL_ID));
    assert!(catalog.models.contains_key(MODEL_ID));
    save_provider_credentials(
        &StateDir::resolve().unwrap(),
        SLUG,
        &ProviderCredentials {
            api_key: SAVED_KEY.to_owned(),
            host: None,
        },
    )
    .unwrap();
    let (base_url, requests) = model_server();
    unsafe { env::set_var(base_url_env_var(SLUG), base_url) };
    let _runtime = Runtime::new();
    let mut model = Model::from_spec(MODEL).unwrap();
    let provider = from_model(&mut model, Timeouts::default()).unwrap();
    let models = smol::block_on(provider.list_models()).unwrap();
    assert!(models.iter().all(|model| model.id != OBSOLETE_MODEL_ID));
    model_registry::set_known_models(SLUG, models);

    let model = Model::from_spec(MODEL).unwrap();
    assert_eq!(model.context_window, LIVE_CONTEXT);
    assert_eq!(model.max_output_tokens, Some(LIVE_CONTEXT));
    assert_eq!(
        model.pricing,
        ModelPricing::per_million(0.1, 0.4, 0.0, 0.01)
    );
    assert!(model.supports_thinking());
    assert!(model.supports_vision());
    let policy = maki_config::ModelPolicy::new(&[format!("{SLUG}/*")], &[]).unwrap();
    assert!(
        available_model_specs(&policy)
            .iter()
            .all(|spec| spec != &format!("{SLUG}/{OBSOLETE_MODEL_ID}"))
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, MODELS_PATH);
    assert_eq!(requests[0].authorization(), SAVED_AUTHORIZATION);
}

#[test_case(Some(GATEWAY_KEY), None, GATEWAY_KEY ; "gateway_key")]
#[test_case(None, Some(LEGACY_KEY), LEGACY_KEY ; "legacy_key")]
#[test_case(Some(GATEWAY_KEY), Some(LEGACY_KEY), GATEWAY_KEY ; "gateway_takes_priority")]
#[test_case(Some(EMPTY_KEY), Some(LEGACY_KEY), LEGACY_KEY ; "empty_gateway_uses_legacy")]
fn declared_key_reaches_authenticated_discovery(
    gateway: Option<&str>,
    legacy: Option<&str>,
    expected: &str,
) {
    let _home = isolated_state(true);
    for (name, key) in [(KEY_ENV, gateway), (LEGACY_KEY_ENV, legacy)] {
        if let Some(key) = key {
            unsafe { env::set_var(name, key) };
        }
    }
    let (base_url, requests) = model_server();
    unsafe { env::set_var(base_url_env_var(SLUG), base_url) };
    let _runtime = Runtime::new();
    let provider = plugin::create(SLUG, Timeouts::default()).unwrap();
    smol::block_on(provider.list_models()).unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].authorization(), format!("Bearer {expected}"));
}
