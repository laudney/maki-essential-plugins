use super::{PACKAGE, Runtime, WAIT_LIMIT, Window, isolated_state};
use maki_providers::plugin;
use maki_storage::id::MakiId;
use serde_json::json;
use std::sync::Arc;
use test_case::test_case;

const FIRST: &str = "alpha/first";
const SECOND: &str = "alpha/second";
const OTHER: &str = "beta/vendor/model";
const UNLISTED: &str = "beta/new/model";
const SWITCH_ERROR: &str = "Model is not allowed by policy";

impl Runtime {
    fn with_providers() -> Self {
        let runtime = Self::new();
        {
            let mut models = runtime.models.lock().unwrap();
            models.current = FIRST.into();
            models.available = vec![OTHER.into(), SECOND.into(), FIRST.into(), SECOND.into()];
        }
        runtime
    }

    fn provider_command(&self, args: &str) {
        self.host.event_handle().run_command(
            Arc::from(PACKAGE),
            Arc::from("/provider"),
            args.into(),
            0,
        );
    }

    fn provider_picker(&self, args: &str) -> Window {
        self.provider_command(args);
        self.windows.recv_timeout(WAIT_LIMIT).unwrap()
    }

    fn provider_notice(&self) -> String {
        self.flashes.recv_timeout(WAIT_LIMIT).unwrap()
    }

    fn selected_models(&self) -> Vec<String> {
        self.models.lock().unwrap().selected.clone()
    }
}

#[test]
fn switch_providers_and_back_through_the_real_lua_host() {
    let runtime = Runtime::with_providers();
    let providers = runtime.provider_picker("");
    assert_eq!(providers.cursor(), 2);
    providers.key("<Down>");
    assert_eq!(providers.cursor(), 3);
    providers.key("<CR>");
    providers.closed();
    let models = runtime.windows.recv_timeout(WAIT_LIMIT).unwrap();
    assert_eq!(models.cursor(), 2);
    models.key("<CR>");
    models.closed();
    assert_eq!(runtime.provider_notice(), format!("Selected {OTHER}"));

    let models = runtime.provider_picker("alpha");
    assert_eq!(models.cursor(), 2);
    models.key("<Down>");
    assert_eq!(models.cursor(), 3);
    models.key("<CR>");
    models.closed();
    assert_eq!(runtime.provider_notice(), format!("Selected {SECOND}"));
    assert_eq!(runtime.selected_models(), [OTHER, SECOND]);
}

#[test]
fn filtering_and_backspace_select_the_model_and_preserve_the_current_pick() {
    let runtime = Runtime::with_providers();
    let models = runtime.provider_picker("alpha");
    assert_eq!(models.cursor(), 2);
    models.key("<Down>");
    assert_eq!(models.cursor(), 3);
    models.key("s");
    assert_eq!(models.cursor(), 2);
    models.key("e");
    assert_eq!(models.cursor(), 2);
    models.key("x");
    assert_eq!(models.cursor(), 2);
    models.key("<CR>");
    assert_eq!(models.cursor(), 2);
    assert!(runtime.selected_models().is_empty());
    models.key("<BS>");
    assert_eq!(models.cursor(), 2);
    models.key("<CR>");
    models.closed();
    assert_eq!(runtime.provider_notice(), format!("Selected {SECOND}"));

    let models = runtime.provider_picker("alpha");
    assert_eq!(models.cursor(), 3);
    models.key("<Esc>");
    models.closed();
    assert_eq!(runtime.selected_models(), [SECOND]);
}

#[test]
fn explicit_model_specs_forward_the_full_route_and_report_core_errors() {
    let runtime = Runtime::with_providers();
    runtime.provider_command(&format!("  {UNLISTED}  "));
    assert_eq!(runtime.provider_notice(), format!("Selected {UNLISTED}"));
    runtime.models.lock().unwrap().error = Some(SWITCH_ERROR.into());
    runtime.provider_command(FIRST);
    assert_eq!(
        runtime.provider_notice(),
        format!("Provider switch failed: {SWITCH_ERROR}")
    );
    assert_eq!(runtime.selected_models(), [UNLISTED]);
    assert!(runtime.windows.try_recv().is_err());
}

#[test]
fn a_missing_provider_or_empty_discovery_reports_how_to_connect() {
    let runtime = Runtime::with_providers();
    runtime.provider_command("missing");
    let notice = runtime.provider_notice();
    assert!(notice.contains("maki auth login missing"), "{notice}");
    assert!(runtime.selected_models().is_empty());

    runtime.models.lock().unwrap().available.clear();
    runtime.provider_command("");
    let notice = runtime.provider_notice();
    assert!(notice.contains("open /model to refresh"), "{notice}");
    assert!(runtime.windows.try_recv().is_err());
}

#[test]
fn cancelling_either_picker_leaves_the_model_unchanged() {
    let runtime = Runtime::with_providers();
    let providers = runtime.provider_picker("");
    providers.key("<Esc>");
    providers.closed();
    let models = runtime.provider_picker("alpha");
    models.key("<Esc>");
    models.closed();
    assert!(runtime.selected_models().is_empty());
    assert_eq!(runtime.models.lock().unwrap().current, FIRST);
}

#[test]
fn a_focus_change_closes_the_picker_before_it_can_switch_another_session() {
    let runtime = Runtime::with_providers();
    let models = runtime.provider_picker("alpha");
    assert_eq!(models.cursor(), 2);
    *runtime.focused.lock().unwrap() = MakiId::generate();
    runtime.fire("SessionFocusChanged", json!({}));
    models.closed();
    assert!(runtime.selected_models().is_empty());
}

#[test]
fn session_end_and_package_unload_close_the_picker() {
    let runtime = Runtime::with_providers();
    let models = runtime.provider_picker("alpha");
    assert_eq!(models.cursor(), 2);
    runtime.fire("SessionEnd", json!({}));
    models.closed();
    assert!(runtime.selected_models().is_empty());

    let models = runtime.provider_picker("beta");
    assert_eq!(models.cursor(), 2);
    runtime.host.unload(PACKAGE).unwrap();
    models.closed();
    assert!(runtime.selected_models().is_empty());
}

#[test_case(&[] ; "no_provider_names_approved")]
#[test_case(&["runinfra"] ; "runinfra_only")]
#[test_case(&["fireworks-ai"] ; "fireworks_only")]
#[test_case(&["runinfra", "fireworks-ai"] ; "both_manifest_names")]
fn manifest_provider_names_require_an_explicit_grant(granted: &[&str]) {
    let _home = isolated_state();
    let runtime = Runtime::with_provider_grants(granted);
    assert!(runtime.registry.get("get_goal").is_some());
    assert!(runtime.registry.get("monitor").is_some());
    for slug in ["runinfra", "fireworks-ai"] {
        assert_eq!(
            plugin::is_registered(slug),
            granted.contains(&slug),
            "{slug}"
        );
    }
}
