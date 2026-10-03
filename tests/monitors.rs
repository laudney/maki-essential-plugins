use super::{Gate, Runtime};
use serde_json::json;

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
