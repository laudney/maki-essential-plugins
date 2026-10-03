use super::{Runtime, wait_until};
use maki_agent::SessionMailbox;
use maki_storage::id::MakiId;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

#[test]
fn cancellation_leaves_goal_interrupted_until_explicit_resume() {
    for reason in ["cancelled", "dropped"] {
        let runtime = Runtime::new();
        runtime.goal_running();
        runtime.fire("TurnEnd", json!({"reason": reason}));
        wait_until(|| runtime.hints().contains("Goal interrupted"));
        assert!(runtime.notices().is_empty());
        let original = runtime.goal();
        assert_eq!(original["status"], "active");
        let error = runtime.call("update_goal", json!({"goal_id": original["id"], "execution_id": original["execution_id"], "status": "complete", "summary": "Done"})).unwrap_err();
        assert!(error.contains("not running"), "{error}");
        runtime.command("resume");
        assert_ne!(runtime.goal()["execution_id"], original["execution_id"]);
        assert_eq!(runtime.notices().len(), 1);
    }
}

#[test]
fn finished_turn_continues_and_terminal_update_stops_goal() {
    let runtime = Runtime::new();
    runtime.goal_running();
    runtime.fire("TurnEnd", json!({"reason": "finished"}));
    let mut notices = Vec::new();
    wait_until(|| {
        notices.extend(runtime.notices());
        !notices.is_empty()
    });
    assert!(notices[0].contains("Continue working"));
    runtime.fire("TurnStart", json!({"text": "continue"}));
    let goal = runtime.goal();
    let result = runtime.call("update_goal", json!({"goal_id": goal["id"], "execution_id": goal["execution_id"], "status": "complete", "summary": "Verified"})).unwrap();
    assert!(result.contains("Verified"));
    runtime.fire("TurnEnd", json!({"reason": "finished"}));
    wait_until(|| runtime.hints().contains("Goal achieved"));
    assert!(runtime.notices().is_empty());
}

#[test]
fn failed_turn_blocks_goal_and_pause_stops_continuation() {
    let runtime = Runtime::new();
    runtime.goal_running();
    runtime.fire("TurnError", json!({"message": "Provider unavailable"}));
    wait_until(|| runtime.hints().contains("Goal blocked"));
    assert_eq!(runtime.goal()["summary"], "Provider unavailable");
    runtime.command("resume");
    runtime.notices();
    runtime.fire("TurnStart", json!({}));
    runtime.command("pause");
    runtime.fire("TurnEnd", json!({"reason": "finished"}));
    assert!(runtime.hints().contains("Goal paused"));
    assert!(runtime.notices().is_empty());
}

#[test]
fn delivery_failure_blocks_the_saved_goal() {
    let mut runtime = Runtime::new();
    runtime.mailbox = SessionMailbox::register(MakiId::generate());
    assert!(runtime.command("Ship the feature").contains("Goal error"));
    assert_eq!(runtime.goal()["status"], "blocked");
    assert!(
        runtime.goal()["summary"]
            .as_str()
            .unwrap()
            .contains("Goal delivery failed")
    );
}

#[test]
fn corrupt_state_is_reported_without_overwriting_it() {
    let runtime = Runtime::new();
    runtime.goal_running();
    let path = PathBuf::from(std::env::var("XDG_STATE_HOME").unwrap())
        .join("maki/goals/sessions")
        .join(format!(
            "{}.json",
            runtime.ctx.session_id.as_ref().unwrap().as_str()
        ));
    fs::write(&path, "broken JSON").unwrap();
    assert!(
        runtime
            .call("get_goal", json!({}))
            .unwrap_err()
            .contains("decode")
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "broken JSON");
}

#[test]
fn focus_and_background_reset_preserve_the_focused_goal_hint() {
    let runtime = Runtime::new();
    runtime.goal_running();
    runtime.host.event_handle().fire_autocmd(
        "SessionReset",
        json!({"session_id": MakiId::generate().to_string()}),
    );
    runtime.host.event_handle().collect_prompt_slots();
    assert!(runtime.hints().contains("Pursuing goal"));
    let other = MakiId::generate();
    *runtime.focused.lock().unwrap() = other;
    runtime.host.event_handle().fire_autocmd(
        "SessionFocusChanged",
        json!({"session_id": other.to_string()}),
    );
    wait_until(|| runtime.hints().is_empty());
}
