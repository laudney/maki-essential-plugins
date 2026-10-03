use super::{Gate, PACKAGE, Runtime, wait_until};
use maki_agent::tools::ToolRegistry;
use maki_lua::{Permission, PluginHost, PluginPermissions};
use maki_storage::id::{MakiId, SessionRef};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn reload_preserves_goal_state_without_resuming_or_retaining_monitors() {
    let gate = Gate::new();
    let runtime = Runtime::new();
    runtime.goal_running();
    runtime
        .call("monitor", json!({"command": gate.command()}))
        .unwrap();
    runtime.host.unload(PACKAGE).unwrap();
    assert!(runtime.hints().is_empty());
    let package = PathBuf::from(std::env::var("MAKI_ESSENTIAL_PACKAGE").unwrap());
    runtime
        .host
        .load_package(
            PACKAGE,
            &package,
            PluginPermissions::from_approved(["fs_read", "fs_write", "run"]),
            Default::default(),
        )
        .unwrap();
    assert!(
        runtime
            .call("get_goal", json!({}))
            .unwrap()
            .contains("interrupted")
    );
    assert_eq!(
        runtime.call("monitor_list", json!({})).unwrap(),
        "no monitors running"
    );
    assert!(runtime.notices().is_empty());
}

#[test]
fn monitor_reports_output_and_real_exit_once_without_colliding_with_goal_hint() {
    let runtime = Runtime::new();
    runtime.goal_running();
    for command in [
        "printf 'hello'; exit 7",
        "exec sh -c 'printf hello; exit 7'",
        "set -e; printf hello; false",
    ] {
        let expected = if command.starts_with("set") {
            "exited with 1"
        } else {
            "exited with 7"
        };
        runtime
            .call(
                "monitor",
                json!({"command": command, "label": "test", "wake": true}),
            )
            .unwrap();
        let mut notices = Vec::new();
        wait_until(|| {
            notices.extend(runtime.notices());
            notices.iter().any(|text| text.contains(expected))
        });
        assert_eq!(
            notices
                .iter()
                .filter(|text| text.contains("exited with"))
                .count(),
            1,
            "{notices:?}"
        );
        assert!(
            notices.iter().any(|text| text == "[test] hello"),
            "{notices:?}"
        );
        wait_until(|| !runtime.hints().contains("monitor"));
        assert!(runtime.hints().contains("Pursuing goal"));
    }
}

#[test]
fn output_cannot_impersonate_exit_marker_and_patterns_use_lua_rules() {
    let runtime = Runtime::new();
    assert!(
        runtime
            .call("monitor", json!({"command": "true", "match": "a%%|b"}))
            .unwrap_err()
            .contains("alternation")
    );
    assert!(
        runtime
            .call("monitor", json!({"command": "true", "match": "["}))
            .unwrap_err()
            .contains("invalid match")
    );
    runtime.call("monitor", json!({"command": "printf '__maki_monitor_exit__ 99\\n'; printf '__maki_monitor_exit__ 88\\n' >&2; exit 3", "label": "marker"})).unwrap();
    let mut notices = Vec::new();
    wait_until(|| {
        notices.extend(runtime.notices());
        notices.iter().any(|text| text.contains("exited with 3"))
    });
    assert!(
        notices.contains(&"[marker] __maki_monitor_exit__ 99".into()),
        "{notices:?}"
    );
    assert!(
        notices.contains(&"[marker] stderr: __maki_monitor_exit__ 88".into()),
        "{notices:?}"
    );
    assert_eq!(
        notices
            .iter()
            .filter(|text| text.contains("exited with"))
            .count(),
        1
    );
    for pattern in ["%|", "[|]", "[]|]", "%b||"] {
        runtime
            .call(
                "monitor",
                json!({"command": "printf '|x|\\n'", "label": "pattern", "match": pattern}),
            )
            .unwrap();
        let mut matched = Vec::new();
        wait_until(|| {
            matched.extend(runtime.notices());
            matched.iter().any(|text| text == "[pattern] |x|")
        });
    }
}

#[test]
fn waking_is_opt_in_and_a_command_exit_can_wake() {
    for wake in [false, true] {
        let runtime = Runtime::new();
        runtime
            .call("monitor", json!({"command": "true", "wake": wake}))
            .unwrap();
        wait_until(|| runtime.call("monitor_list", json!({})).unwrap() == "no monitors running");
        assert_eq!(runtime.mailbox.claim_wake().len(), usize::from(wake));
        if !wake {
            assert_eq!(runtime.notices().len(), 1);
        }
    }
}

#[test]
fn a_pattern_error_discovered_in_output_is_reported_once_and_can_wake() {
    for wake in [false, true] {
        let runtime = Runtime::new();
        runtime
            .call(
                "monitor",
                json!({"command": "printf 'ERROR\\nERROR\\n'", "match": "ERROR[", "wake": wake}),
            )
            .unwrap();
        wait_until(|| runtime.call("monitor_list", json!({})).unwrap() == "no monitors running");
        assert_eq!(runtime.mailbox.claim_wake().len(), if wake { 2 } else { 0 });
        if !wake {
            assert_eq!(runtime.notices().len(), 2);
        }
    }
}

#[test]
fn picker_uses_upstream_keys_and_can_stop_the_selected_monitor() {
    let gate = Gate::new();
    let runtime = Runtime::new();
    runtime.goal_running();
    for label in ["first", "second"] {
        runtime
            .call(
                "monitor",
                json!({"command": gate.command(), "label": label}),
            )
            .unwrap();
    }
    assert!(runtime.hints().contains("Pursuing goal"));
    assert!(runtime.hints().contains("2 monitors"));
    let window = runtime.picker();
    assert_eq!(window.cursor(), 1);
    window.key("<Down>");
    assert_eq!(window.cursor(), 2);
    window.key("<Up>");
    assert_eq!(window.cursor(), 1);
    window.key("<PageDown>");
    assert_eq!(window.cursor(), 2);
    window.key("d");
    assert_eq!(window.cursor(), 1);
    let listing = runtime.call("monitor_list", json!({})).unwrap();
    assert!(listing.contains("first"), "{listing}");
    assert!(!listing.contains("second"), "{listing}");
    window.key("<Esc>");
    window.closed();
    assert!(runtime.hints().contains("Pursuing goal"));
}

#[test]
fn picker_closes_on_focus_change_even_if_keys_keep_arriving() {
    let gate = Gate::new();
    let runtime = Runtime::new();
    runtime
        .call("monitor", json!({"command": gate.command()}))
        .unwrap();
    let window = runtime.picker();
    assert_eq!(window.cursor(), 1);
    let other = MakiId::generate();
    *runtime.focused.lock().unwrap() = other;
    runtime.host.event_handle().fire_autocmd(
        "SessionFocusChanged",
        json!({"session_id": other.to_string()}),
    );
    wait_until(|| runtime.hints().is_empty());
    window.key("d");
    window.closed();
    assert!(
        runtime
            .call("monitor_list", json!({}))
            .unwrap()
            .contains("monitor")
    );
}

#[test]
fn session_cleanup_and_foreign_stop_cannot_affect_another_session() {
    let gate = Gate::new();
    let mut runtime = Runtime::new();
    let original = runtime.ctx.session_id.clone();
    let output = runtime
        .call(
            "monitor",
            json!({"command": gate.command(), "label": "owned"}),
        )
        .unwrap();
    let id: u32 = output
        .split("(id ")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    runtime.ctx.session_id = Some(SessionRef::generate());
    assert_eq!(
        runtime.call("monitor_list", json!({})).unwrap(),
        "no monitors running"
    );
    assert!(
        runtime
            .call("monitor_stop", json!({"id": id}))
            .unwrap_err()
            .contains("no monitor")
    );
    runtime.ctx.session_id = original;
    assert!(
        runtime
            .call("monitor_list", json!({}))
            .unwrap()
            .contains("owned")
    );
    runtime.fire("SessionEnd", json!({"reason": "reset"}));
    wait_until(|| runtime.call("monitor_list", json!({})).unwrap() == "no monitors running");
}

#[test]
fn monitor_forces_command_approval_and_requires_run_permission() {
    let runtime = Runtime::new();
    let tool = runtime.registry.get("monitor").unwrap();
    assert_eq!(tool.tool.required_permission(), Some(Permission::Run));
    let invocation = tool.tool.parse(&json!({"command": "true"})).unwrap();
    let scopes = smol::block_on(invocation.permission_scopes()).unwrap();
    assert_eq!(scopes.scopes, ["true"]);
    assert!(scopes.force_prompt);
    let host = PluginHost::new(Arc::new(ToolRegistry::new())).unwrap();
    let package = PathBuf::from(std::env::var("MAKI_ESSENTIAL_PACKAGE").unwrap());
    let error = host
        .load_package(
            PACKAGE,
            &package,
            PluginPermissions::denied(),
            Default::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("run"), "{error}");
}

#[test]
fn output_limit_does_not_hide_exit() {
    let runtime = Runtime::new();
    let command =
        "i=0; while [ \"$i\" -lt 201 ]; do printf 'progress\\n'; i=$((i+1)); done; exit 9";
    runtime
        .call("monitor", json!({"command": command, "label": "limit"}))
        .unwrap();
    let mut notices = Vec::new();
    wait_until(|| {
        notices.extend(runtime.notices());
        notices.iter().any(|text| text == "[limit] exited with 9")
    });
    assert!(
        notices
            .iter()
            .any(|text| text.contains("stopped reporting after 200 lines")),
        "{notices:?}"
    );
    assert!(
        !notices
            .iter()
            .any(|text| text.contains("__maki_monitor_exit__"))
    );
}

#[test]
fn explicit_exit_reports_before_a_background_child_closes_the_pipes() {
    let gate = Gate::new();
    let runtime = Runtime::new();
    let command = format!(
        "sh -c \"{}\" &\nprintf 'finished\\n'; exit 7",
        gate.command()
    );
    runtime
        .call(
            "monitor",
            json!({"command": command, "label": "parent", "match": "never", "wake": true}),
        )
        .unwrap();
    let mut notices = Vec::new();
    wait_until(|| {
        notices.extend(runtime.notices());
        notices.iter().any(|text| text == "[parent] exited with 7")
    });
    assert_eq!(notices, ["[parent] exited with 7"]);
    assert!(
        runtime
            .call("monitor_list", json!({}))
            .unwrap()
            .contains("parent")
    );
}
