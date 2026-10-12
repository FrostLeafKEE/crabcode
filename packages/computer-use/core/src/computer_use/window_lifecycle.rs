//! Window identity is never repaired by matching a title, PID or rectangle.
use serde_json::{json, Value};

fn same_identity(left: &Value, right: &Value) -> bool {
    left["id"] == right["id"] && left["pid"] == right["pid"]
}

pub(super) fn stale_before_action(id: &str, windows: &[Value]) -> Value {
    let mut result = json!({
        "ok": false, "error_code": "target_stale",
        "error": format!("Window not found: {id}. List windows and observe a new target before acting"),
        "summary": "Target window is stale; no input was sent",
        "action_dispatched": false, "dispatch_succeeded": false,
        "effect_verified": false, "retry_safe": true, "requires_observation": true,
    });
    record(&mut result, id, windows, Ok(windows));
    result["window_lifecycle"]["phase"] = json!("before_action");
    result
}

pub(super) fn record(
    result: &mut Value,
    id: &str,
    before: &[Value],
    after: Result<&[Value], &str>,
) {
    result["requested_window_id"] = json!(id);
    let after = match after {
        Ok(after) => after,
        Err(error) => {
            result["window_lifecycle"] =
                json!({"target_resolvable": null, "snapshot_error": error});
            result["requires_observation"] = json!(true);
            return;
        }
    };
    let original = before.iter().find(|w| w["id"].as_str() == Some(id));
    let relevant = |window: &&Value| original.is_none_or(|root| root["pid"] == window["pid"]);
    let resolvable =
        original.is_some_and(|original| after.iter().any(|w| same_identity(original, w)));
    let appeared: Vec<_> = after
        .iter()
        .filter(relevant)
        .filter(|w| !before.iter().any(|old| same_identity(old, w)))
        .collect();
    let disappeared: Vec<_> = before
        .iter()
        .filter(relevant)
        .filter(|w| !after.iter().any(|new| same_identity(w, new)))
        .collect();
    result["window_lifecycle"] = json!({
        "phase": "after_action", "target_resolvable": resolvable,
        "appeared_windows": appeared, "disappeared_windows": disappeared,
        "focused_window_id": unique_focused_window(after),
        "requires_observation": !resolvable || !appeared.is_empty() || !disappeared.is_empty(),
    });
    if !resolvable || !appeared.is_empty() || !disappeared.is_empty() {
        result["requires_observation"] = json!(true);
    }
    if !resolvable {
        result.as_object_mut().unwrap().remove("screenshot");
        // A delivered Enter/click may normally dismiss a sheet. Preserve the
        // successful dispatch, without claiming the save/rename succeeded.
        // A reused ID or a failed/uncertain dispatch is still a stale error.
        if original.is_some()
            && result["ok"] == true
            && result["action_dispatched"] == true
            && !after.iter().any(|w| w["id"].as_str() == Some(id))
        {
            result["target_disappeared_after_action"] = json!(true);
            result["retry_safe"] = json!(false);
            result["summary"] = json!("Input was sent and the target is no longer visible; observe the parent window to verify the result");
            return;
        }
        if result.get("error_code").is_some() && result["error_code"] != "target_stale" {
            result["execution_error_code"] = result["error_code"].clone();
        }
        result["ok"] = json!(false);
        result["error_code"] = json!("target_stale");
        if original.is_some() {
            result["error"] = json!("The target disappeared or changed identity after the action. Observe a new target; the action was not replayed");
            result["summary"] = json!(match result["action_dispatched"].as_bool() {
                Some(true) => "Input was sent; the original target is now stale",
                Some(false) => "No input was sent; the original target is now stale",
                None => "The target is now stale; input delivery is uncertain",
            });
        }
    }
}

fn unique_focused_window(windows: &[Value]) -> Option<&str> {
    let mut focused = windows.iter().filter(|w| w["focused"] == true);
    let first = focused.next()?;
    focused
        .next()
        .is_none()
        .then(|| first["id"].as_str())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_is_reported_without_replaying_or_clearing_dispatch() {
        let before = vec![json!({"id": "1", "pid": 7, "title": "same", "focused": true})];
        let after = vec![json!({"id": "2", "pid": 7, "title": "same", "focused": true})];
        for dispatched in [json!(true), json!(false), Value::Null] {
            let mut result =
                json!({"ok": true, "action_dispatched": dispatched, "retry_safe": false});
            record(&mut result, "1", &before, Ok(&after));
            if dispatched == true {
                assert_eq!(result["ok"], true);
                assert_eq!(result["target_disappeared_after_action"], true);
                assert_eq!(result["error_code"], Value::Null);
            } else {
                assert_eq!(result["error_code"], "target_stale");
            }
            assert_eq!(result["action_dispatched"], dispatched);
            assert_eq!(result["window_lifecycle"]["appeared_windows"][0]["id"], "2");
            assert_eq!(
                result["window_lifecycle"]["disappeared_windows"][0]["id"],
                "1"
            );
            assert_eq!(result["window_lifecycle"]["focused_window_id"], "2");
            assert_eq!(result["requires_observation"], true);
        }
        let stale = stale_before_action("1", &after);
        assert_eq!(stale["action_dispatched"], false);
        assert_eq!(stale["window_lifecycle"]["phase"], "before_action");
    }

    #[test]
    fn unrelated_windows_and_ambiguous_focus_do_not_mislead_the_caller() {
        let root = json!({"id": "1", "pid": 7, "focused": true});
        let cursor = json!({"id": "9", "pid": 99});
        let mut result = json!({"ok": true, "action_dispatched": true});
        record(
            &mut result,
            "1",
            &[root.clone(), cursor],
            Ok(&[root.clone()]),
        );
        assert_eq!(result["window_lifecycle"]["requires_observation"], false);
        assert_eq!(result["window_lifecycle"]["disappeared_windows"], json!([]));
        assert_eq!(
            unique_focused_window(&[root.clone(), json!({"id": "2", "focused": true})]),
            None
        );
        assert_eq!(unique_focused_window(&[root]), Some("1"));
        assert_eq!(
            unique_focused_window(&[json!({"id": "1", "focused": null})]),
            None
        );
    }

    #[test]
    fn new_window_and_unavailable_snapshot_do_not_claim_target_disappeared() {
        let before = vec![json!({"id": "1", "pid": 7})];
        let mut after = before.clone();
        after.push(json!({"id": "2", "pid": 7}));
        let mut result = json!({"ok": true, "action_dispatched": true});
        record(&mut result, "1", &before, Ok(&after));
        assert_eq!(result["ok"], true);
        assert_eq!(result["window_lifecycle"]["target_resolvable"], true);
        assert_eq!(result["requires_observation"], true);
        record(&mut result, "1", &before, Err("enumeration failed"));
        assert_eq!(result["window_lifecycle"]["target_resolvable"], Value::Null);
        assert_eq!(result["action_dispatched"], true);
        let reused = vec![json!({"id": "1", "pid": 8})];
        record(&mut result, "1", &before, Ok(&reused));
        assert_eq!(result["error_code"], "target_stale");
    }
}
