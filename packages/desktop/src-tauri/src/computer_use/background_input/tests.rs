use super::*;

fn candidate(pid: i32) -> Profile {
    Profile {
        identity: identity(pid).unwrap(),
        actions: vec![
            "click".into(),
            "type".into(),
            "keypress".into(),
            "scroll".into(),
        ],
        shortcuts: Vec::new(),
        pointer: Route::SkyLight,
        scroll: None,
        keyboard: Route::Quartz,
        prepare_active: true,
        primer: false,
        semantic_click: false,
        ax_actions: Vec::new(),
    }
}

fn window_roles(pid: i32) -> Value {
    let app = mac_ax_relations::application(pid).unwrap();
    match mac_ax_copy_attribute(&app, "AXWindows") {
        Ok(value) => json!(value.downcast::<CFArray>().map(|arr| arr
            .get_all_values()
            .iter()
            .map(|ptr| {
                let node = unsafe { CFType::wrap_under_get_rule((*ptr).cast()) };
                json!({"id":mac_ax_window_id(&node),"role":mac_ax_string(&node,"AXRole")})
            })
            .collect::<Vec<_>>())),
        Err(status) => json!({"status":status}),
    }
}

fn write_receipt(path: &std::path::Path, mut receipt: Value) {
    if let Some(frame) = receipt.as_object_mut().unwrap().remove("screenshot") {
        if let Some(encoded) = frame["data"].as_str() {
            std::fs::write(
                path.with_extension("png"),
                STANDARD.decode(encoded).unwrap(),
            )
            .unwrap();
        }
    }
    std::fs::write(path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    eprintln!("background_receipt={receipt}");
}

#[test]
fn profiles_do_not_generalize_across_os_app_or_gesture() {
    let id = Identity {
        os_build: "test-os".into(),
        architecture: "aarch64".into(),
        bundle_id: "test.app".into(),
        app_version: "1".into(),
        app_build: "1".into(),
    };
    let profile = Profile {
        identity: id.clone(),
        actions: vec!["click".into()],
        shortcuts: Vec::new(),
        pointer: Route::SkyLight,
        scroll: None,
        keyboard: Route::Quartz,
        prepare_active: true,
        primer: false,
        semantic_click: false,
        ax_actions: Vec::new(),
    };
    let action: ComputerAction = serde_json::from_value(json!({"action":"click"})).unwrap();
    assert!(supported(&profile, &id, &action));
    let mut changed = id.clone();
    changed.app_build = "2".into();
    assert!(!supported(&profile, &changed, &action));
    changed = id.clone();
    changed.os_build = "new-os".into();
    assert!(!supported(&profile, &changed, &action));
    let right = serde_json::from_value(json!({"action":"click", "button":"right"})).unwrap();
    assert!(!supported(&profile, &id, &right));
    let drag = serde_json::from_value(json!({"action":"drag"})).unwrap();
    assert!(!supported(&profile, &id, &drag));
}

#[test]
fn rejection_does_not_claim_delivery_or_invite_automatic_retry() {
    let action = serde_json::from_value(json!({"action":"click"})).unwrap();
    let result = rejected(&action, "unverified target");
    assert_eq!(result["ok"], false);
    assert_eq!(result["action_dispatched"], false);
    assert_eq!(result["input_method"], "none");
    assert!(result.get("required_delivery_policy").is_none());
}

#[test]
#[ignore = "requires macOS input/capture permissions; isolated AppKit host; run serially"]
fn native_background_provider_delivers_and_cleans_up() {
    mac_require_input_permission().unwrap();
    let host = super::super::tests::MacInputTestHost::start_fixture(
        "tests/fixtures/scroll_host.swift",
        &["--require-active", "--separate-windows"],
    );
    let state = (0..200)
        .find_map(|_| {
            let s = host.state();
            if s.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            s
        })
        .unwrap();
    let target = window_target(&state["target_id"].to_string()).unwrap();
    let profile = candidate(target.pid);
    eprintln!("native_ax_before={}", window_roles(target.pid));
    let action = serde_json::from_value(json!({"action":"click", "window_id": target.window_id.to_string(),
        "x": state["x"].as_i64().unwrap() - i64::from(target.x), "y": state["y"].as_i64().unwrap() - i64::from(target.y), "include_screenshot": false})).unwrap();
    let result = run(&action, target, &profile);
    eprintln!("native_background_receipt={result}");
    let after = host.state().unwrap();
    eprintln!("native_ax_after={}", window_roles(target.pid));
    eprintln!(
        "native_background_effect={}",
        json!({"clicks":after["target_clicks"],"decoy_clicks":after["decoy_clicks"],"active":after["active"],"ever_frontmost":after["ever_frontmost"],"cursor":after["cursor"]})
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["focus_isolation"], "preserved");
    assert_eq!(result["cleanup_succeeded"], true);
    assert_eq!(after["target_clicks"], 1);
    assert_eq!(after["decoy_clicks"], 0);
    assert_eq!(after["active"], false);
    assert_eq!(after["ever_frontmost"], false);
    assert_eq!(after["cursor"], state["cursor"]);
}

/// Explicit local probe input, never exposed through the production tool schema.
/// Allows inspecting a selected app or exercising a candidate before its profile
/// is checked in. Receipts go to a caller-owned temporary path, not chat history.
#[test]
#[ignore = "manual validation only; requires CRAB_BACKGROUND_PROBE JSON file"]
fn live_background_probe() {
    let path = std::env::var("CRAB_BACKGROUND_PROBE").expect("probe input path required");
    let config: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    if let Some(steps) = config["steps"].as_array() {
        for step in steps {
            let mut single = config.clone();
            single.as_object_mut().unwrap().remove("steps");
            single
                .as_object_mut()
                .unwrap()
                .extend(step.as_object().unwrap().clone());
            probe_once(single);
        }
    } else {
        probe_once(config);
    }
}

fn probe_once(config: Value) {
    let pid = config["pid"].as_i64().unwrap() as i32;
    let app_identity = identity(pid).unwrap();
    let windows: Vec<_> = window_list()
        .unwrap()
        .into_iter()
        .filter(|w| w["pid"].as_i64() == Some(i64::from(pid)))
        .collect();
    let output = std::path::Path::new(config["output"].as_str().unwrap());
    if config["action"].is_null() {
        let mut result = json!({"identity": app_identity, "windows":windows});
        if let Some(id) = config["inspect_window"].as_str() {
            let _observer = AccessibilityLease::start(pid).unwrap();
            let target = window_target(id).unwrap();
            let app = mac_ax_relations::application(pid).unwrap();
            enable_accessibility(&app);
            let focus = mac_ax_relations::focus_snapshot(&app, pid);
            result["focus"] =
                json!({"window":focus.focused_window_id,"role":focus.focused_element_role});
            let window = match mac_ax_window(&app, target) {
                Ok(window) => window,
                Err(error) => {
                    result["ax_window_error"] = json!(error);
                    result["ax_windows"] = match mac_ax_copy_attribute(&app,"AXWindows") {
                        Ok(value) => json!(value.downcast::<CFArray>().map(|arr|arr.get_all_values().iter().map(|ptr| {
                            let node = unsafe { CFType::wrap_under_get_rule((*ptr).cast()) };
                            json!({"id":mac_ax_window_id(&node),"role":mac_ax_string(&node,"AXRole"),"title":mac_ax_string(&node,"AXTitle")})
                        }).collect::<Vec<_>>())),
                        Err(status) => json!({"status":status}),
                    };
                    write_receipt(output, result);
                    return;
                }
            };
            let mut pending = std::collections::VecDeque::from([(window, 0)]);
            let mut visited = Vec::new();
            let mut elements = Vec::new();
            while let Some((node, depth)) = pending.pop_front() {
                if visited.contains(&node) || depth > 25 || visited.len() >= 2000 {
                    continue;
                }
                visited.push(node.clone());
                let role = mac_ax_string(&node, "AXRole").unwrap_or_default();
                if config["fixture_state"].as_bool() == Some(true) {
                    if let Some(value) =
                        mac_ax_string(&node, "AXValue").filter(|s| s.starts_with("{\"clicks\":"))
                    {
                        result["fixture_state"] = serde_json::from_str(&value).unwrap();
                    }
                }
                if matches!(
                    role.as_str(),
                    "AXButton"
                        | "AXTextField"
                        | "AXTextArea"
                        | "AXSearchField"
                        | "AXTab"
                        | "AXRadioButton"
                ) {
                    let title = mac_ax_string(&node, "AXTitle")
                        .or_else(|| mac_ax_string(&node, "AXDescription"));
                    if config["needle"]
                        .as_str()
                        .is_none_or(|needle| title.as_ref().is_some_and(|t| t.contains(needle)))
                    {
                        let frame = mac_ax_frame(&node);
                        elements.push(json!({"role": role,"title":title,"frame":frame.map(|f|json!([f.origin.x,f.origin.y,f.size.width,f.size.height]))}));
                    }
                }
                pending.extend(
                    mac_ax_children(&node)
                        .into_iter()
                        .map(|child| (child, depth + 1)),
                );
            }
            result["elements"] = json!(elements);
        }
        write_receipt(output, result);
        return;
    }
    let action: ComputerAction = serde_json::from_value(config["action"].clone()).unwrap();
    let target = window_target(action.window_id.as_deref().unwrap()).unwrap();
    assert_eq!(target.pid, pid);
    let mut profile = candidate(pid);
    if let Some(value) = config["prepare_active"].as_bool() {
        profile.prepare_active = value;
    }
    if let Some(value) = config["primer"].as_bool() {
        profile.primer = value;
    }
    if let Some(value) = config["semantic_click"].as_bool() {
        profile.semantic_click = value;
    }
    if !config["pointer"].is_null() {
        profile.pointer = serde_json::from_value(config["pointer"].clone()).unwrap();
    }
    if !config["keyboard"].is_null() {
        profile.keyboard = serde_json::from_value(config["keyboard"].clone()).unwrap();
    }
    let mut receipt = if config["production"].as_bool() == Some(true) {
        super::super::execute(serde_json::from_value(json!({"target_scope":"app_window","delivery_policy":"strict_background","action": config["action"]})).unwrap()).unwrap()
    } else {
        run(&action, target, &profile)
    };
    receipt["probe_window_state"] = json!(mac_all_window_info()
        .unwrap()
        .iter()
        .filter(|w| w.target.pid == pid && w.target.window_id == target.window_id)
        .map(|w| json!({"id":w.target.window_id,"title":w.title,"on_screen":w.on_screen}))
        .collect::<Vec<_>>());
    if let Some(expected) = config["expected_focused_value"].as_str() {
        let _observer = AccessibilityLease::start(pid).unwrap();
        let app = mac_ax_relations::application(pid).unwrap();
        let actual = mac_ax_copy_attribute(&app, "AXFocusedUIElement")
            .ok()
            .and_then(|node| mac_ax_string(&node, "AXValue"));
        receipt["probe_effect"] = json!({"expected":expected,"actual":actual,"matches":actual.as_deref()==Some(expected)});
    }
    write_receipt(output, receipt.clone());
    if let Some(expected) = config["expect"].as_object() {
        for (pointer, value) in expected {
            assert_eq!(
                receipt.pointer(pointer),
                Some(value),
                "receipt assertion {pointer}; inspect {}",
                output.display()
            );
        }
    }
}

#[test]
fn production_profiles_exclude_unverified_input() {
    assert!(!profiles().is_empty());
    for profile in profiles() {
        for action in ["type", "keypress", "scroll", "move", "drag", "open_app"] {
            let action =
                serde_json::from_value(json!({"action":action,"text":"test","keys":["CMD","A"]}))
                    .unwrap();
            assert!(!supported(profile, &profile.identity, &action));
        }
        assert!(profile.shortcuts.is_empty());
    }
}

#[test]
fn activation_cleanup_record_addresses_the_same_window() {
    let active = activation_record(0x12345678, true);
    let inactive = activation_record(0x12345678, false);
    assert_eq!(&active[60..64], &0x12345678u32.to_le_bytes());
    assert_eq!(active[138], 1);
    assert_eq!(inactive[138], 2);
    assert_eq!(
        active
            .iter()
            .zip(inactive)
            .filter(|(a, b)| **a != *b)
            .count(),
        1
    );
}

#[test]
fn window_order_guards_new_dialogs_without_rejecting_existing_tooltips_or_closure() {
    use std::collections::BTreeMap;
    let window = |id, pid| MacWindowInfo {
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
        on_screen: true,
    };
    // Existing tooltip 3 is above user window 1; main target 2 is below it.
    let above = BTreeMap::from([(3, vec![]), (2, vec![1])]);
    let below = BTreeMap::from([(3, vec![1, 5]), (2, vec![5])]);
    let check = |windows: Vec<_>| check_window_order(&windows, 10, &[1], &above, &below);
    let mut popup = window(4, 10);
    popup.layer = 3;
    assert!(check(vec![popup, window(1, 20), window(2, 10), window(5, 30)]).is_err());
    assert!(check(vec![
        window(3, 10),
        window(1, 20),
        window(2, 10),
        window(5, 30)
    ])
    .is_ok());
    assert!(check(vec![window(3, 10), window(1, 20), window(5, 30)]).is_ok());
    assert!(check(vec![
        window(3, 10),
        window(4, 10),
        window(1, 20),
        window(2, 10),
        window(5, 30)
    ])
    .is_err());
    assert!(check(vec![
        window(3, 10),
        window(1, 20),
        window(4, 10),
        window(2, 10),
        window(5, 30)
    ])
    .is_ok());
    assert!(check(vec![
        window(3, 10),
        window(1, 20),
        window(5, 30),
        window(2, 10)
    ])
    .is_err());
}
