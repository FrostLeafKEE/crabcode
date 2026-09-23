use super::*;

#[test]
fn foreground_process_is_not_reported_as_multiple_focused_windows() {
    let info = |id, pid, on_screen| MacWindowInfo {
        target: WindowTarget {
            window_id: id,
            pid,
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        },
        app_name: String::new(),
        title: String::new(),
        layer: 0,
        on_screen,
    };
    let windows = [
        info(1, 7, true),
        info(2, 7, true),
        info(3, 8, true),
        info(4, 7, false),
    ];
    let listed = mac_window_list_values(&windows, Some(7), Some(2));
    assert_eq!(listed.len(), 3);
    assert_eq!(listed.iter().filter(|w| w["focused"] == true).count(), 1);
    assert_eq!(listed[0]["application_frontmost"], true);
    assert_eq!(listed[0]["focused"], false);
    assert_eq!(listed[1]["focused"], true);
    let unknown = mac_window_list_values(&windows, Some(7), None);
    assert_eq!(unknown[0]["focused"], Value::Null);
    assert_eq!(unknown[2]["focused"], false);
    assert!(mac_window_list_values(&windows, None, None)
        .iter()
        .all(|w| w["focused"].is_null()));
}

#[test]
fn timings_are_per_request_and_do_not_change_receipts() {
    let result = diagnostics::measure(|| {
        let _stage = diagnostics::Stage::new("fixture");
        Ok(json!({"ok": false, "action_dispatched": null}))
    })
    .unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["action_dispatched"], Value::Null);
    assert!(result["timings_ms"]["native_total"].as_f64().unwrap() >= 0.0);
    assert!(result["timings_ms"]["fixture"].is_number());
    let next = diagnostics::measure(|| Ok(json!({}))).unwrap();
    assert!(next["timings_ms"]["fixture"].is_null());
}

#[test]
#[ignore = "requires Accessibility and Screen Recording; opens an isolated real NSSavePanel"]
fn macos_real_save_sheet_receives_one_ax_click_and_saves_file() {
    mac_require_input_permission().expect("Accessibility permission required");
    let host = tests::MacInputTestHost::start_fixture("tests/fixtures/save_panel_host.swift", &[]);
    let state = (0..400)
        .find_map(|_| {
            let state = host
                .state()
                .filter(|s| s["panel"].as_u64().is_some_and(|id| id > 0));
            if state.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            state
        })
        .unwrap_or_else(|| panic!("save fixture did not open: {:?}", host.state()));
    thread::sleep(Duration::from_millis(300));
    let target = window_target(&state["panel"].to_string()).unwrap();
    let application = mac_ax_relations::application(target.pid).unwrap();
    let window = mac_ax_window(&application, target).unwrap();
    let listed = window_list().unwrap();
    let focused: Vec<_> = listed.iter().filter(|w| w["focused"] == true).collect();
    assert!(
        focused.len() <= 1,
        "multiple windows reported focused: {focused:?}"
    );
    if let Some(focused) = focused.first() {
        assert_eq!(
            mac_process_serial_number(focused["pid"].as_i64().unwrap() as i32).unwrap(),
            mac_front_process_serial_number().unwrap()
        );
    }

    let mut pending = vec![window];
    let mut visited = Vec::new();
    let button = loop {
        let element = pending
            .pop()
            .expect("save button absent from real NSSavePanel");
        if visited.contains(&element) {
            continue;
        }
        assert!(visited.len() < 512);
        visited.push(element.clone());
        if mac_ax_string(&element, "AXRole").as_deref() == Some("AXButton")
            && mac_ax_string(&element, "AXTitle").as_deref() == Some("Save fixture")
        {
            break element;
        }
        pending.extend(mac_ax_children(&element));
    };
    let frame = mac_ax_frame(&button).unwrap();
    let relations = mac_ax_relations::snapshot(&application, target.pid).relations;
    let sibling = window_target(&state["sibling"].to_string()).unwrap();
    assert!(mac_ax_element_in_target(&button, target, &relations));
    assert!(!mac_ax_element_in_target(&button, sibling, &relations));
    let x = (frame.origin.x + frame.size.width / 2.0).round() as i32 - target.x;
    let y = (frame.origin.y + frame.size.height / 2.0).round() as i32 - target.y;
    let hit = mac_ax_click_target(&application, target, x + target.x, y + target.y).unwrap();
    assert_eq!(mac_ax_string(&hit, "AXRole").as_deref(), Some("AXButton"));
    assert_eq!(
        mac_ax_string(&hit, "AXTitle").as_deref(),
        Some("Save fixture")
    );
    let result = execute(
        serde_json::from_value(json!({
            "target_scope": "app_window", "delivery_policy": "allow_foreground",
            "action": {"action": "click", "window_id": target.window_id.to_string(),
                "x": x, "y": y, "include_screenshot": false}
        }))
        .unwrap(),
    )
    .unwrap();
    eprintln!("save_sheet_receipt={result}");
    assert_eq!(result["action_dispatched"], true);
    assert_eq!(result["input_method"], "accessibility_action");
    let completed = (0..100)
        .find_map(|_| {
            let state = host.state().filter(|s| s["completed"] == true);
            if state.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            state
        })
        .expect("save callback was not received");
    let saved = std::path::Path::new(completed["saved_path"].as_str().unwrap());
    assert_eq!(saved.parent(), host.state_path.parent());
    assert_eq!(
        std::fs::read_to_string(saved).unwrap(),
        "line 1\nline 2\nline 3\n"
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["target_disappeared_after_action"], true);
    assert_eq!(result["retry_safe"], false);
    assert!(window_list()
        .unwrap()
        .iter()
        .any(|w| w["id"] == state["sibling"].to_string()));
}
