use super::*;

fn owner(session: &str) -> AxOwner {
    AxOwner {
        host_id: "host".into(),
        connection_id: "connection".into(),
        session_id: session.into(),
        agent_id: None,
    }
}

#[test]
fn release_is_scoped_to_connection_session_and_agent() {
    let a = owner("a");
    assert!(release_matches(
        &a,
        "host",
        "connection",
        Some("a"),
        None,
        false
    ));
    assert!(!release_matches(
        &a,
        "host",
        "new-connection",
        Some("a"),
        None,
        true
    ));
    assert!(!release_matches(
        &a,
        "host",
        "connection",
        Some("b"),
        None,
        true
    ));
    let mut child = a.clone();
    child.agent_id = Some("child".into());
    assert!(!release_matches(
        &child,
        "host",
        "connection",
        Some("a"),
        None,
        false
    ));
    assert!(release_matches(
        &child,
        "host",
        "connection",
        Some("a"),
        None,
        true
    ));
    assert!(release_matches(
        &child,
        "host",
        "connection",
        None,
        None,
        true
    ));
}

#[test]
fn uncertain_dispatch_cannot_be_replayed_as_coordinate_input() {
    let result = dispatch_result("press", "AXPress", AX_ERROR_CANNOT_COMPLETE);
    assert_eq!(result["action_dispatched"], Value::Null);
    assert_eq!(result["retry_safe"], false);
    assert_eq!(result["error_code"], "ax_dispatch_uncertain");
    let unsupported = dispatch_result("press", "AXPress", AX_ERROR_ACTION_UNSUPPORTED);
    assert_eq!(unsupported["action_dispatched"], false);
    let delivered = dispatch_result("press", "AXPress", AX_ERROR_SUCCESS);
    assert_eq!(delivered["action_dispatched"], true);
    assert_eq!(delivered["effect_verified"], false);
}

#[test]
fn strict_rules_do_not_generalize_to_other_controls() {
    let rules = vec![background_input::AxRule {
        role: "AXButton".into(),
        subrole: Some("AXCloseButton".into()),
        operation: "AXPress".into(),
    }];
    assert!(background_input::ax_allowed(
        &rules,
        "AXButton",
        Some("AXCloseButton"),
        "AXPress"
    ));
    assert!(!background_input::ax_allowed(
        &rules, "AXButton", None, "AXPress"
    ));
    assert!(!background_input::ax_allowed(
        &rules,
        "AXButton",
        Some("AXCloseButton"),
        "AXShowMenu"
    ));
    assert!(secure("AXTextField", Some("AXSecureTextField")));
}

#[test]
fn validates_element_input_before_any_native_dispatch() {
    let make = |scope: &str, action: Value| {
        serde_json::from_value::<ExecuteRequest>(json!({
        "target_scope": scope,
        "owner": {"host_id":"host", "connection_id":"connection", "session_id":"session", "agent_id":null},
        "action": action,
    })).unwrap()
    };
    let action = json!({"action":"press", "window_id":"1", "snapshot_id":"s", "element_id":"e1"});
    let req = make("app_window", action.clone());
    assert!(validate_ax_request(&req, TargetScope::AppWindow).is_none());
    assert!(validate_ax_request(&req, TargetScope::Desktop).is_some());
    let mut mixed = action;
    mixed["x"] = json!(10);
    assert!(validate_ax_request(&make("app_window", mixed), TargetScope::AppWindow).is_some());
    let mut empty = make(
        "app_window",
        json!({"action":"set_value", "window_id":"1", "snapshot_id":"s", "element_id":"e1", "text":""}),
    );
    assert!(validate_ax_request(&empty, TargetScope::AppWindow).is_none());
    empty.owner = None;
    assert!(validate_ax_request(&empty, TargetScope::AppWindow).is_some());
}

#[test]
fn anonymous_layout_does_not_hide_document_content_or_controls() {
    let wrapper = json!({"role":"AXGroup", "enabled":true});
    let generic = vec!["AXShowMenu".into(), "AXScrollToVisible".into()];
    assert!(!expose_node(&wrapper, &generic, false, false));
    assert!(expose_node(&wrapper, &generic, false, true));
    assert!(expose_node(&wrapper, &["AXPress".into()], false, false));
    assert!(expose_node(
        &json!({"role":"AXGroup", "description":"Chat"}),
        &generic,
        false,
        false
    ));
    assert!(expose_node(
        &json!({"role":"AXStaticText", "value":"Message body"}),
        &generic,
        false,
        false
    ));
    assert!(expose_node(&json!({"role":"AXTextArea"}), &[], true, false));
    assert!(expose_node(
        &json!({"role":"AXGroup", "protected":true}),
        &[],
        false,
        false
    ));
    assert!(expose_node(
        &json!({"role":"AXButton", "enabled":false}),
        &[],
        false,
        false
    ));
}

#[test]
#[ignore = "read-only live probe; requires an explicitly selected CRABCODE_AX_PROBE_WINDOW_ID"]
fn macos_ax_observes_selected_live_window_without_images_or_input() {
    let id = std::env::var("CRABCODE_AX_PROBE_WINDOW_ID").expect("select a live window explicitly");
    let mut previous_snapshot = Value::Null;
    for policy in ["strict_background", "allow_foreground"] {
        let started = Instant::now();
        let result = execute(serde_json::from_value(json!({
            "target_scope":"app_window", "delivery_policy":policy,
            "owner":{"host_id":"ax-probe", "connection_id":"read-only", "session_id":"probe", "agent_id":null},
            "action":{"action":"observe", "observation":"ax", "window_id":id}
        })).unwrap()).unwrap();
        assert_eq!(result["ok"], true, "{}", result["error"]);
        assert_eq!(result["action_dispatched"], false);
        assert!(result.get("screenshot").is_none());
        let tree = &result["accessibility"];
        assert_ne!(tree["snapshot_id"], previous_snapshot);
        previous_snapshot = tree["snapshot_id"].clone();
        let elements = tree["elements"].as_array().unwrap();
        if let Ok(expected) = std::env::var("CRABCODE_AX_PROBE_EXPECT_TEXT") {
            assert!(
                elements.iter().any(
                    |node| ["title", "value", "description"]
                        .iter()
                        .any(|field| node[*field]
                            .as_str()
                            .is_some_and(|text| text.contains(&expected)))
                ),
                "Expected visible content is absent from the AX observation"
            );
        }
        eprintln!(
            "AX probe: policy={policy}, nodes={}, visited={}, truncated={}, elapsed_ms={}",
            elements.len(),
            tree["visited_nodes"],
            tree["truncated"],
            started.elapsed().as_millis()
        );
        if let Ok(path) = std::env::var("CRABCODE_AX_PROBE_OUTPUT") {
            std::fs::write(path, serde_json::to_vec(&result).unwrap()).unwrap();
        }
    }
}

#[test]
#[ignore = "requires Accessibility; reads and edits only isolated native test windows"]
fn macos_ax_tree_edits_by_reference_and_rejects_stale_and_cross_session_input() {
    mac_require_input_permission().expect("Accessibility permission required");
    let host = super::super::tests::MacInputTestHost::start_fixture(
        "tests/fixtures/keyboard_host.swift",
        &[],
    );
    let initial = (0..200)
        .find_map(|_| {
            let state = host.state();
            if state.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            state
        })
        .expect("fixture ready");
    let id = initial["first"].to_string();
    let request = |session: &str, policy: &str, mut action: Value| {
        action["window_id"] = json!(id);
        execute(serde_json::from_value(json!({"target_scope":"app_window", "delivery_policy":policy,
            "owner":{"host_id":"ax-test", "connection_id":"fixture", "session_id":session, "agent_id":null}, "action":action})).unwrap()).unwrap()
    };
    let observed = request(
        "a",
        "allow_foreground",
        json!({"action":"observe", "observation":"ax"}),
    );
    eprintln!("ax_observation={observed}");
    assert_eq!(observed["ok"], true);
    assert!(observed.get("screenshot").is_none());
    assert!(observed.get("preview_screenshot").is_some() || observed.get("preview_screenshot_error").is_some());
    let tree = &observed["accessibility"];
    let editor = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["role"] == "AXTextArea")
        .expect("editor exposed");
    assert_eq!(editor["value_settable"], true);
    let set = json!({"action":"set_value", "snapshot_id":tree["snapshot_id"], "element_id":editor["element_id"], "text":"AX 编辑成功\nsecond line"});
    let foreign = request("b", "allow_foreground", set.clone());
    assert_eq!(foreign["action_dispatched"], false);
    assert_eq!(foreign["error_code"], "ax_reference_stale");
    let result = request("a", "allow_foreground", set.clone());
    eprintln!("ax_set_value={result}");
    assert_eq!(result["ok"], true);
    assert_eq!(result["effect_verified"], true);
    assert_eq!(result["verification_method"], "ax_value");
    assert!(result.get("screenshot").is_none());
    let after = (0..100)
        .find_map(|_| {
            let state = host.state()?;
            if state["first_text"] == "AX 编辑成功\nsecond line" {
                Some(state)
            } else {
                thread::sleep(Duration::from_millis(30));
                None
            }
        })
        .expect("native editor changed");
    assert_eq!(after["second_text"], "second");
    let stale = request("a", "allow_foreground", set);
    assert_eq!(stale["error_code"], "ax_reference_stale");
    assert_eq!(stale["action_dispatched"], false);
    let observed = request(
        "a",
        "strict_background",
        json!({"action":"observe", "observation":"ax"}),
    );
    let tree = &observed["accessibility"];
    let editor = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["role"] == "AXTextArea")
        .unwrap();
    assert_eq!(editor["allow_set_value"], false);
    let rejected = request(
        "a",
        "strict_background",
        json!({"action":"set_value", "snapshot_id":tree["snapshot_id"], "element_id":editor["element_id"], "text":"must not be sent"}),
    );
    assert_eq!(rejected["error_code"], "background_delivery_unsupported");
    assert_eq!(rejected["action_dispatched"], false);
    let observed = request(
        "a",
        "allow_foreground",
        json!({"action":"observe", "observation":"ax"}),
    );
    let tree = &observed["accessibility"];
    let editor = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["role"] == "AXTextArea")
        .unwrap();
    let set = json!({"action":"set_value", "snapshot_id":tree["snapshot_id"], "element_id":editor["element_id"], "text":"must not be sent"});
    let screenshot = request(
        "a",
        "allow_foreground",
        json!({"action":"observe", "observation":"screenshot"}),
    );
    assert_eq!(screenshot["observation_kind"], "screenshot");
    assert!(screenshot["screenshot"]["data"].is_string());
    assert_eq!(
        request("a", "allow_foreground", set)["error_code"],
        "ax_reference_stale"
    );
    let observed = request(
        "a",
        "allow_foreground",
        json!({"action":"observe", "observation":"ax"}),
    );
    let tree = &observed["accessibility"];
    let editor = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["role"] == "AXTextArea")
        .unwrap();
    release("ax-test", "fixture", Some("a"), None, false);
    let released = request(
        "a",
        "allow_foreground",
        json!({"action":"set_value", "snapshot_id":tree["snapshot_id"], "element_id":editor["element_id"], "text":"must not be sent"}),
    );
    assert_eq!(released["error_code"], "ax_reference_stale");
    assert_eq!(
        host.state().unwrap()["first_text"],
        "AX 编辑成功\nsecond line"
    );
    let mut snapshot = Value::Null;
    for action in [
        json!({"action":"focus_window"}),
        json!({"action":"keypress", "keys":["RIGHT"]}),
        json!({"action":"scroll", "x":200, "y":150, "delta_y":100}),
    ] {
        let result = request("a", "allow_foreground", action);
        assert_eq!(result["ok"], true, "{}", result["error"]);
        assert_eq!(result["observation_kind"], "ax");
        assert!(result.get("screenshot").is_none());
        assert!(result["accessibility"]["elements"].is_array());
        assert_ne!(result["accessibility"]["snapshot_id"], snapshot);
        snapshot = result["accessibility"]["snapshot_id"].clone();
    }
    let with_pixels = request(
        "a",
        "allow_foreground",
        json!({"action":"keypress", "keys":["LEFT"], "include_screenshot":true}),
    );
    assert_eq!(with_pixels["observation_kind"], "ax_and_screenshot");
    assert!(with_pixels["screenshot"]["data"].is_string());
    assert_eq!(
        host.state().unwrap()["first_text"],
        "AX 编辑成功\nsecond line"
    );
}

#[test]
#[ignore = "requires Accessibility and Screen Recording; observes only an isolated AX-empty window"]
fn macos_ax_empty_auto_observation_falls_back_to_window_screenshot() {
    let host = super::super::tests::MacInputTestHost::start_fixture(
        "tests/fixtures/keyboard_host.swift",
        &["--empty-ax"],
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
    let result = execute(serde_json::from_value(json!({"target_scope":"app_window", "delivery_policy":"strict_background",
        "owner":{"host_id":"empty-test", "connection_id":"fixture", "session_id":"empty", "agent_id":null},
        "action":{"action":"observe", "window_id":state["first"].to_string()}})).unwrap()).unwrap();
    assert_eq!(result["ok"], true, "{}", result["error"]);
    assert_eq!(result["observation_kind"], "screenshot");
    assert_eq!(result["fallback_reason"], "ax_empty");
    assert_eq!(result["action_dispatched"], false);
    assert_eq!(result["delivery_policy"], "strict_background");
    assert!(result["screenshot"]["data"].is_string());
}

#[test]
#[ignore = "requires Accessibility and Chrome; edits and clicks only a local page in a disposable browser profile"]
fn macos_chrome_ax_write_and_explicit_pixel_input_update_local_page() {
    let directory = tempfile::tempdir().unwrap();
    let previous: Vec<_> = window_list()
        .unwrap()
        .iter()
        .filter_map(|w| w["pid"].as_i64())
        .collect();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/background_page.html");
    let url = url::Url::from_file_path(fixture).unwrap();
    struct Cleanup {
        launch: std::process::Child,
        pid: Option<i32>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(pid) = self.pid {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
            let _ = self.launch.kill();
            let _ = self.launch.wait();
        }
    }
    let mut cleanup = Cleanup {
        launch: Command::new("open")
            .args([
                "-n",
                "-g",
                "-W",
                "-a",
                "Google Chrome",
                "--args",
                "--no-first-run",
                "--no-default-browser-check",
            ])
            .arg(format!("--user-data-dir={}", directory.path().display()))
            .arg(format!("--app={url}"))
            .spawn()
            .unwrap(),
        pid: None,
    };
    let target = (0..100)
        .find_map(|_| {
            let windows = window_list().ok()?;
            let window = windows.iter().find(|w| {
                w["title"]
                    .as_str()
                    .is_some_and(|t| t.starts_with("CrabBG c="))
                    && w["pid"]
                        .as_i64()
                        .is_some_and(|pid| !previous.contains(&pid))
            });
            if let Some(window) = window {
                window_target(window["id"].as_str()?).ok()
            } else {
                thread::sleep(Duration::from_millis(100));
                None
            }
        })
        .expect("isolated Chrome app window");
    cleanup.pid = Some(target.pid);
    let send = |mut action: Value| {
        action["window_id"] = json!(target.window_id.to_string());
        execute(serde_json::from_value(json!({"target_scope":"app_window", "delivery_policy":"allow_foreground",
            "owner":{"host_id":"chrome-test", "connection_id":"fixture", "session_id":"chrome", "agent_id":null}, "action":action})).unwrap()).unwrap()
    };
    // Establish the permitted foreground precondition before observing and
    // dispatching. An acknowledged AXPress can still have no visible effect.
    let focused = send(json!({"action":"focus_window", "include_screenshot":false}));
    assert_eq!(focused["ok"], true, "{focused}");
    let observed = send(json!({"action":"observe", "observation":"ax"}));
    assert_eq!(observed["ok"], true, "{observed}");
    let tree = &observed["accessibility"];
    let editor = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| {
            node["role"] == "AXTextField"
                && (node["title"] == "Background test text"
                    || node["description"] == "Background test text")
        })
        .unwrap_or_else(|| panic!("Chrome input absent: {tree}"));
    let filled = send(
        json!({"action":"set_value", "snapshot_id":tree["snapshot_id"], "element_id":editor["element_id"], "text":"AX中文"}),
    );
    assert_eq!(filled["ok"], true, "{filled}");
    assert_eq!(filled["effect_verified"], true, "{filled}");
    let tree = &filled["accessibility"];
    let button = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["role"] == "AXButton" && node["title"] == "Count exactly one click")
        .expect("Chrome button");
    let pressed = send(
        json!({"action":"press", "snapshot_id":tree["snapshot_id"], "element_id":button["element_id"]}),
    );
    assert_eq!(pressed["ok"], true, "{pressed}");
    assert_eq!(pressed["action_dispatched"], true);
    assert_eq!(pressed["effect_verified"], false);
    assert_eq!(pressed["retry_safe"], false);
    assert!(pressed["verification_warning"].is_string());
    let page_state = |observation: &Value| {
        observation["accessibility"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|node| node["value"].as_str())
            .find_map(|value| {
                serde_json::from_str::<Value>(value)
                    .ok()
                    .filter(|v| v.get("clicks").is_some())
            })
            .expect("local page counter")
    };
    // Chrome can acknowledge a press without firing a DOM click. Read again
    // without any input, rather than assuming success or automatically replaying.
    thread::sleep(Duration::from_millis(500));
    let observed = send(json!({"action":"observe", "observation":"ax"}));
    let state = page_state(&observed);
    let before = state["clicks"].as_u64().unwrap();
    assert!(before <= 1, "AX must not dispatch duplicate input: {state}");
    assert_eq!(state["text"], "AX中文", "{observed}");
    assert!(pressed.get("screenshot").is_none());
    eprintln!("chrome_ax_acknowledged_business_state={state}");

    // Make a separate, deliberate pixel-based decision after a fresh screenshot.
    // These window-local coordinates target the fixed CSS button in this new,
    // default-zoom fixture profile, not an AX-to-pixel coordinate conversion.
    let screenshot = send(json!({"action":"observe", "observation":"screenshot"}));
    assert!(screenshot["screenshot"]["data"].is_string());
    let clicked = send(json!({"action":"click", "x":150, "y":164}));
    assert_eq!(clicked["action_dispatched"], true, "{clicked}");
    assert_eq!(clicked["input_method"], "quartz_event", "{clicked}");
    assert_eq!(clicked["observation_kind"], "ax");
    assert!(clicked.get("screenshot").is_none());
    let observed = send(json!({"action":"observe", "observation":"ax"}));
    let state = page_state(&observed);
    assert_eq!(state["clicks"], before + 1, "{observed}");
    assert_eq!(state["text"], "AX中文");
    eprintln!("chrome_explicit_pixel_business_state={state}");
}

#[test]
#[ignore = "requires Accessibility; opens and edits only a disposable document in a new TextEdit instance"]
fn macos_textedit_strict_ax_value_preserves_isolation() {
    mac_require_input_permission().expect("Accessibility permission required");
    // Use a known, steady foreground responder rather than sampling the user's
    // actively changing editor. Both this control app and TextEdit are disposable.
    let control = super::super::tests::MacInputTestHost::start_fixture(
        "tests/fixtures/keyboard_host.swift",
        &[],
    );
    let state = (0..200)
        .find_map(|_| {
            let state = control.state();
            if state.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            state
        })
        .expect("foreground control fixture");
    let control_target = window_target(&state["first"].to_string()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let name = format!(
        "CrabCode-AX-{}-{}.txt",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let path = directory.path().join(&name);
    std::fs::write(&path, "Disposable AX validation document\n").unwrap();
    let previous: Vec<_> = window_list()
        .unwrap()
        .iter()
        .filter_map(|w| w["pid"].as_i64())
        .collect();
    struct Cleanup {
        launch: std::process::Child,
        pid: Option<i32>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(pid) = self.pid {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
            let _ = self.launch.kill();
            let _ = self.launch.wait();
        }
    }
    let mut cleanup = Cleanup {
        launch: Command::new("open")
            .args(["-n", "-g", "-W", "-a", "/System/Applications/TextEdit.app"])
            .arg(&path)
            .args(["--args", "-ApplePersistenceIgnoreState", "YES"])
            .spawn()
            .unwrap(),
        pid: None,
    };
    let target = (0..100)
        .find_map(|_| {
            let windows = window_list().ok()?;
            let window = windows.iter().find(|w| {
                w["title"].as_str().is_some_and(|t| t.contains(&name))
                    && w["pid"]
                        .as_i64()
                        .is_some_and(|pid| !previous.contains(&pid))
            });
            if let Some(window) = window {
                window_target(window["id"].as_str()?).ok()
            } else {
                thread::sleep(Duration::from_millis(100));
                None
            }
        })
        .expect("new disposable TextEdit window");
    cleanup.pid = Some(target.pid);
    // App launches and the previous fixture's shutdown may switch focus. Pin
    // the control responder only after the target has finished launching.
    assert!(
        mac_prepare_mouse_window(control_target).is_ok(),
        "activate only the isolated control window"
    );
    thread::sleep(Duration::from_millis(300));
    let send = |action: Value| {
        execute(serde_json::from_value(json!({
        "target_scope":"app_window", "delivery_policy":"strict_background",
        "owner":{"host_id":"textedit-test", "connection_id":"fixture", "session_id":"textedit", "agent_id":null}, "action":action,
    })).unwrap()).unwrap()
    };
    let observed = send(
        json!({"action":"observe", "window_id":target.window_id.to_string(), "observation":"ax"}),
    );
    assert_eq!(observed["ok"], true, "{observed}");
    let tree = &observed["accessibility"];
    let editor = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["role"] == "AXTextArea" && e["value_settable"] == true)
        .expect("TextEdit writable AX text area");
    assert_eq!(
        editor["allow_set_value"], true,
        "TextEdit needs an exact validated AX profile on this machine"
    );
    let result = send(
        json!({"action":"set_value", "window_id":target.window_id.to_string(),
        "snapshot_id":tree["snapshot_id"], "element_id":editor["element_id"], "text":"严格后台 AX 写值验证\nline two"}),
    );
    eprintln!("textedit_ax_receipt={result}");
    assert_eq!(result["ok"], true);
    assert_eq!(result["action_dispatched"], true);
    assert_eq!(result["effect_verified"], true);
    assert_eq!(result["foreground_activated"], false);
    assert_eq!(result["focus_isolation"], "preserved");
    assert!(result["isolation_samples"]["samples"].as_u64().unwrap() > 0);
    assert!(result["accessibility"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["role"] == "AXTextArea" && e["value"] == "严格后台 AX 写值验证\nline two"));
}

#[test]
#[ignore = "requires Accessibility; clicks only the button in an isolated native fixture"]
fn macos_ax_press_scopes_elements_to_the_selected_window() {
    mac_require_input_permission().unwrap();
    let host = super::super::tests::MacInputTestHost::start_fixture(
        "tests/fixtures/scroll_host.swift",
        &[],
    );
    let initial = (0..200)
        .find_map(|_| {
            let state = host.state();
            if state.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            state
        })
        .expect("fixture ready");
    let id = initial["target_id"].to_string();
    let send = |mut action: Value| {
        action["window_id"] = json!(id);
        execute(serde_json::from_value(json!({"target_scope":"app_window", "delivery_policy":"allow_foreground",
            "owner":{"host_id":"press-test", "connection_id":"fixture", "session_id":"press", "agent_id":null}, "action":action})).unwrap()).unwrap()
    };
    let observed = send(json!({"action":"observe", "observation":"ax"}));
    assert_eq!(observed["ok"], true, "{observed}");
    let tree = &observed["accessibility"];
    let nodes = tree["elements"].as_array().unwrap();
    assert!(
        !nodes.iter().any(|node| node["title"] == "AX click"),
        "Sibling window leaked into the tree"
    );
    let button = nodes
        .iter()
        .find(|node| node["title"] == "Test click")
        .expect("native button in AX tree");
    let press = json!({"action":"press", "snapshot_id":tree["snapshot_id"], "element_id":button["element_id"]});
    let result = send(press.clone());
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["input_method"], "accessibility_action");
    assert_eq!(result["ax_change_detected"], true);
    let after = (0..100)
        .find_map(|_| {
            let state = host.state()?;
            if state["button_presses"] == 1 {
                Some(state)
            } else {
                thread::sleep(Duration::from_millis(30));
                None
            }
        })
        .expect("button callback");
    assert_eq!(after["decoy_button_presses"], 0);
    assert_eq!(after["received_mouse_points"], json!([]));
    let stale = send(press);
    assert_eq!(stale["action_dispatched"], false);
    assert_eq!(host.state().unwrap()["button_presses"], 1);
}
