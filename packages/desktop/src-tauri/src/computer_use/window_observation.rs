//! Independent pixel observations of visible auxiliary windows. Observation
//! does not establish ownership or authorize redirecting input from the root.
use super::*;

const MAX_WINDOWS: usize = 3;

fn candidates(
    frame: &Value,
    root: WindowTarget,
    windows: &[MacWindowInfo],
) -> Vec<(WindowTarget, Value)> {
    let Some(root_index) = windows
        .iter()
        .position(|w| w.target.window_id == root.window_id && w.target.pid == root.pid)
    else {
        return Vec::new();
    };
    let current_root = windows[root_index].target;
    if frame["origin_x"] != current_root.x
        || frame["origin_y"] != current_root.y
        || frame["width"] != current_root.width
        || frame["height"] != current_root.height
    {
        return Vec::new();
    }
    let mut result = Vec::new();
    for relation in frame["excluded_windows"].as_array().into_iter().flatten() {
        // Inspect an identified auxiliary with no owner. Documents, unknown
        // surfaces and auxiliaries tied to another document remain excluded.
        if relation["excluded_reason"] != "ownership_unproven"
            || !matches!(
                relation["kind"].as_str(),
                Some("auxiliary" | "sheet" | "popover")
            )
            || !relation["owner_window_id"].is_null()
            || relation["relationship_evidence"]
                .as_array()
                .is_some_and(|e| !e.is_empty())
        {
            continue;
        }
        let Some(id) = relation["window_id"]
            .as_str()
            .and_then(|id| id.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(window) = windows[..root_index].iter().find(|w| {
            w.target.window_id == id
                && w.target.pid == root.pid
                && w.on_screen
                && window_intersection_area(w.target, current_root) > 0
        }) else {
            continue;
        };
        if !result
            .iter()
            .any(|(target, _): &(WindowTarget, Value)| target.window_id == id)
        {
            result.push((window.target, relation.clone()));
        }
    }
    result
}

fn still_visible(target: WindowTarget, windows: &[MacWindowInfo]) -> bool {
    windows.iter().any(|w| w.target == target && w.on_screen)
}

pub(super) fn attach(result: &mut Value, root: WindowTarget) {
    let Some(frame) = result
        .get("screenshot")
        .or_else(|| result.get("preview_screenshot"))
    else {
        return;
    };
    if !frame["excluded_windows"]
        .as_array()
        .is_some_and(|windows| !windows.is_empty())
    {
        return;
    }
    let _timing = diagnostics::Stage::new("auxiliary_observation");
    let windows = match mac_all_window_info() {
        Ok(windows) => windows,
        Err(error) => {
            result["window_observation_error"] = json!(error);
            return;
        }
    };
    let selected = candidates(frame, root, &windows);
    if selected.is_empty() {
        return;
    }
    // Bound the combined image transport, including a monitor-only root frame.
    let mut remaining =
        (MAX_SCREENSHOT_BYTES / 3 * 4).saturating_sub(frame["data"].as_str().map_or(0, str::len));
    let omitted: Vec<_> = selected
        .iter()
        .skip(MAX_WINDOWS)
        .map(|(target, _)| target.window_id.to_string())
        .collect();
    let mut observations = Vec::new();
    for (target, mut observation) in selected.into_iter().take(MAX_WINDOWS) {
        observation
            .as_object_mut()
            .unwrap()
            .remove("excluded_reason");
        observation["coordinate_space"] = json!("window");
        let capture = (|| {
            // Recheck before and after readback: never revive an ordered-out
            // backing store, moved window or recycled ID as a fresh observation.
            if !still_visible(target, &mac_all_window_info()?) {
                return Err("Auxiliary window changed before capture; observe again".to_string());
            }
            let pixels = mac_capture_window(target)?;
            if !still_visible(target, &mac_all_window_info()?) {
                return Err("Auxiliary window changed during capture; observe again".to_string());
            }
            encode_screenshot(
                pixels,
                target.x,
                target.y,
                format!("window:{}", target.window_id),
            )
        })();
        match capture {
            Ok(frame) if frame["data"].as_str().map_or(0, str::len) <= remaining => {
                remaining -= frame["data"].as_str().map_or(0, str::len);
                observation["observation_kind"] = json!("screenshot");
                observation["screenshot"] = frame;
            }
            Ok(_) => observation["screenshot_error"] = json!("Combined screenshots exceed the 20MB transport limit; observe this window separately"),
            Err(error) => observation["screenshot_error"] = json!(error),
        }
        observations.push(observation);
    }
    if observations.iter().any(|o| o.get("screenshot").is_some()) {
        result["observation_kind"] = json!(if result.get("accessibility").is_some() {
            "ax_and_screenshot"
        } else {
            "screenshot"
        });
    }
    result["window_observations"] = json!(observations);
    if !omitted.is_empty() {
        result["unobserved_window_ids"] = json!(omitted);
    }
    result["requires_observation"] = json!(true);
    result["observation_hint"] = json!("Inspect the independent window_observations before choosing further input. Each screenshot has its own window_id and window-local (0,0) coordinates; it is not composited into the main screenshot. Ownership remains unconfirmed. Explicitly target the window containing the desired control; never redirect a root-window click or reuse a disappeared window ID. Observe any missing window separately.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_observation_never_establishes_ownership_or_uses_stale_surfaces() {
        let root = WindowTarget {
            window_id: 1,
            pid: 10,
            x: 100,
            y: 100,
            width: 300,
            height: 200,
        };
        let window = |id, pid, on_screen| MacWindowInfo {
            target: WindowTarget {
                window_id: id,
                pid,
                ..root
            },
            app_name: String::new(),
            title: String::new(),
            layer: 0,
            on_screen,
        };
        let relation = |id, kind| {
            json!({"window_id":id, "kind":kind, "owner_window_id":null,
            "relationship_evidence":[], "relationship_uncertain":true, "excluded_reason":"ownership_unproven"})
        };
        let mut owned_elsewhere = relation("8", "auxiliary");
        owned_elsewhere["relationship_evidence"] = json!([{"owner_window_id":"99"}]);
        let frame = json!({"origin_x":100,"origin_y":100,"width":300,"height":200,"excluded_windows":[
            relation("2", "auxiliary"), relation("2", "auxiliary"), relation("3", "document"),
            relation("4", "auxiliary"), relation("5", "popover"), relation("6", "unknown"),
            relation("7", "passive"), owned_elsewhere, relation("9", "auxiliary"),
        ]});
        let windows = [
            window(2, 10, true),
            window(3, 10, true),
            window(4, 10, false),
            window(5, 20, true),
            window(6, 10, true),
            window(7, 10, true),
            window(8, 10, true),
            window(1, 10, true),
            window(9, 10, true),
        ];
        let selected = candidates(&frame, root, &windows);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0.window_id, 2);
        assert!(selected[0].1["owner_window_id"].is_null());
        assert_eq!(selected[0].1["relationship_uncertain"], true);
        assert!(!still_visible(
            WindowTarget {
                pid: 11,
                ..selected[0].0
            },
            &windows
        ));
        assert!(!still_visible(
            WindowTarget {
                x: 101,
                ..selected[0].0
            },
            &windows
        ));
        assert!(!still_visible(windows[2].target, &windows));
        let mut moved = frame;
        moved["origin_x"] = json!(99);
        assert!(candidates(&moved, root, &windows).is_empty());
    }

    #[test]
    #[ignore = "types only into an explicitly selected live search field; requires CRABCODE_SEARCH_PROBE window, coordinates, text and output"]
    fn macos_selected_live_search_returns_independent_results() {
        let field = |name| std::env::var(format!("CRABCODE_SEARCH_PROBE_{name}")).unwrap();
        let id = field("WINDOW_ID");
        let x: i32 = field("X").parse().unwrap();
        let y: i32 = field("Y").parse().unwrap();
        let text = field("TEXT");
        let output = field("OUTPUT");
        let run = |mut action: Value| {
            action["window_id"] = json!(id);
            action["include_window_observations"] = json!(true);
            execute(serde_json::from_value(json!({
                "target_scope":"app_window", "delivery_policy":"allow_foreground",
                "owner":{"host_id":"search-probe", "connection_id":"manual", "session_id":"probe", "agent_id":null},
                "action":action
            })).unwrap()).unwrap()
        };
        for action in [
            json!({"action":"click", "x":x, "y":y}),
            json!({"action":"keypress", "keys":["CMD","A"]}),
            json!({"action":"type", "text":text}),
        ] {
            let name = action["action"].as_str().unwrap().to_owned();
            let result = run(action);
            std::fs::write(
                format!("{output}.{name}"),
                serde_json::to_vec(&result).unwrap(),
            )
            .unwrap();
            assert_eq!(result["ok"], true, "{}", result["error"]);
            assert_eq!(result["action_dispatched"], true);
        }
        let result = run(json!({"action":"observe"}));
        std::fs::write(output, serde_json::to_vec(&result).unwrap()).unwrap();
        assert_eq!(result["ok"], true, "{}", result["error"]);
        assert!(
            result["window_observations"]
                .as_array()
                .is_some_and(|windows| {
                    windows
                        .iter()
                        .any(|window| window["screenshot"]["data"].is_string())
                }),
            "independent search results missing"
        );
        let cleared = run(json!({"action":"keypress", "keys":["ESC"]}));
        assert_eq!(cleared["ok"], true, "{}", cleared["error"]);
    }

    #[test]
    #[ignore = "requires Accessibility and Screen Recording; observes and clicks only an isolated popup fixture"]
    fn macos_unowned_popup_is_observed_separately_and_clicked_once() {
        let host = super::super::tests::MacInputTestHost::start_fixture(
            "tests/fixtures/popup_observation_host.swift",
            &[],
        );
        let state = (0..200)
            .find_map(|_| {
                let state = host.state();
                if state.is_none() {
                    thread::sleep(Duration::from_millis(50));
                }
                state
            })
            .expect("fixture ready");
        let id = state["root_id"].to_string();
        let popup_id = state["popup_id"].to_string();
        let run = |action: Value| {
            execute(serde_json::from_value(json!({
            "target_scope":"app_window", "delivery_policy":"allow_foreground",
            "owner":{"host_id":"popup-test", "connection_id":"fixture", "session_id":"test", "agent_id":null}, "action":action,
        })).unwrap()).unwrap()
        };
        let pixels = run(
            json!({"action":"observe","window_id":id,"observation":"screenshot","include_window_observations":true}),
        );
        assert_eq!(pixels["ok"], true, "{}", pixels["error"]);
        let observations = pixels["window_observations"]
            .as_array()
            .expect("independent observations");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0]["window_id"], popup_id);
        assert!(
            observations[0]["screenshot"]["data"].is_string(),
            "{}",
            observations[0]["screenshot_error"]
        );
        assert!(observations[0]["owner_window_id"].is_null());
        assert_eq!(observations[0]["relationship_uncertain"], true);
        assert!(!pixels["screenshot"]["component_window_ids"]
            .as_array()
            .unwrap()
            .contains(&state["popup_id"]));
        let root = window_target(&id).unwrap();
        let popup = window_target(&popup_id).unwrap();
        let point = (
            state["button_x"].as_i64().unwrap() as i32,
            state["button_y"].as_i64().unwrap() as i32,
        );
        assert!(
            mac_background_event_target(root, point).is_err(),
            "observation must not grant root-to-popup input routing"
        );
        let ax = run(json!({"action":"observe","window_id":id,"include_window_observations":true}));
        assert_eq!(ax["observation_kind"], "ax_and_screenshot");
        assert!(ax.get("accessibility").is_some());
        assert!(ax.get("screenshot").is_none());
        assert!(ax.get("preview_screenshot").is_some());
        assert_eq!(ax["window_observations"][0]["window_id"], popup_id);
        let legacy = run(json!({"action":"observe","window_id":id,"observation":"screenshot"}));
        assert!(
            legacy.get("window_observations").is_none(),
            "old Core must not receive unrecognized base64 fields"
        );
        let suppressed = run(
            json!({"action":"observe","window_id":id,"include_screenshot":false,"include_window_observations":true}),
        );
        assert!(suppressed.get("window_observations").is_none());
        if let Ok(path) = std::env::var("CRABCODE_WINDOW_OBSERVATION_OUTPUT") {
            std::fs::write(path, serde_json::to_vec(&pixels).unwrap()).unwrap();
        }
        let clicked = run(
            json!({"action":"click","window_id":popup_id,"x":point.0-popup.x,"y":point.1-popup.y,"include_window_observations":true}),
        );
        assert_eq!(clicked["action_dispatched"], true, "{}", clicked["error"]);
        let after = host.state().unwrap();
        assert_eq!(after["clicks"], 1);
        let dismissed = run(
            json!({"action":"observe","window_id":id,"observation":"screenshot","include_window_observations":true}),
        );
        assert!(dismissed.get("window_observations").is_none());
        let mut old_frame = pixels.clone();
        old_frame
            .as_object_mut()
            .unwrap()
            .remove("window_observations");
        attach(&mut old_frame, root);
        assert!(
            old_frame.get("window_observations").is_none(),
            "hidden retained backing store must not return"
        );
        eprintln!("independent popup observed; root routing rejected; explicit popup received one click; dismissed pixels absent");
    }
}
