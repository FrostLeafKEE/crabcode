//! Window-scoped AX observations and semantic input. Native element handles stay
//! on one worker thread; only bounded JSON and opaque references cross the host.
use super::*;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{mpsc, OnceLock};
use std::time::Instant;

// Rich-text apps split paragraphs into many static text elements. Bound the
// emitted content separately from the larger walk budget for layout wrappers.
const MAX_NODES: usize = 1024;
const MAX_VISITED: usize = 4096;
const MAX_TEXT: usize = 32_000;
const SNAPSHOT_TTL: Duration = Duration::from_secs(120);
const MAX_OWNERS: usize = 32;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementIsAttributeSettable(
        element: CFTypeRef,
        attribute: CFStringRef,
        settable: *mut bool,
    ) -> AXError;
    fn AXUIElementCopyAttributeValues(
        element: CFTypeRef,
        attribute: CFStringRef,
        index: isize,
        count: isize,
        values: *mut CFArrayRef,
    ) -> AXError;
}

pub(super) fn available() -> bool {
    unsafe { AXIsProcessTrusted() }
}

fn require_permission() -> Result<(), String> {
    if available() {
        Ok(())
    } else {
        Err("macOS Accessibility permission is required for AX observation and input".into())
    }
}

enum Job {
    Observe(AxOwner, WindowTarget, DeliveryPolicy, mpsc::Sender<Value>),
    Act(
        AxOwner,
        WindowTarget,
        DeliveryPolicy,
        ComputerAction,
        mpsc::Sender<Value>,
    ),
    Invalidate(AxOwner, mpsc::Sender<()>),
    Release(
        String,
        String,
        Option<String>,
        Option<String>,
        bool,
        mpsc::Sender<()>,
    ),
}

fn worker() -> &'static mpsc::Sender<Job> {
    static WORKER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("computer-use-ax".into())
            .spawn(move || {
                let mut store = Store::default();
                loop {
                    store
                        .snapshots
                        .retain(|_, snapshot| snapshot.created.elapsed() < SNAPSHOT_TTL);
                    let job = match rx.recv_timeout(Duration::from_secs(5)) {
                        Ok(job) => job,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    match job {
                        Job::Observe(owner, target, policy, reply) => {
                            let _ = reply.send(store.observe(owner, target, policy));
                        }
                        Job::Act(owner, target, policy, action, reply) => {
                            let _ = reply.send(store.act(owner, target, policy, action));
                        }
                        Job::Invalidate(owner, reply) => {
                            store.snapshots.remove(&owner);
                            let _ = reply.send(());
                        }
                        Job::Release(host, connection, session, agent, all_agents, reply) => {
                            store.snapshots.retain(|owner, _| {
                                !release_matches(
                                    owner,
                                    &host,
                                    &connection,
                                    session.as_deref(),
                                    agent.as_deref(),
                                    all_agents,
                                )
                            });
                            let _ = reply.send(());
                        }
                    }
                }
            })
            .expect("start accessibility worker");
        tx
    })
}

fn release_matches(
    owner: &AxOwner,
    host: &str,
    connection: &str,
    session: Option<&str>,
    agent: Option<&str>,
    all_agents: bool,
) -> bool {
    owner.host_id == host
        && owner.connection_id == connection
        && session.is_none_or(|session| {
            owner.session_id == session && (all_agents || owner.agent_id.as_deref() == agent)
        })
}

pub(super) fn invalidate(owner: &AxOwner) {
    let (tx, rx) = mpsc::channel();
    if worker().send(Job::Invalidate(owner.clone(), tx)).is_ok() {
        let _ = rx.recv();
    }
}

pub(super) fn release(
    host: &str,
    connection: &str,
    session: Option<&str>,
    agent: Option<&str>,
    all_agents: bool,
) {
    let (tx, rx) = mpsc::channel();
    if worker()
        .send(Job::Release(
            host.into(),
            connection.into(),
            session.map(str::to_owned),
            agent.map(str::to_owned),
            all_agents,
            tx,
        ))
        .is_ok()
    {
        let _ = rx.recv();
    }
}

pub(super) fn observe(request: &ExecuteRequest, target: WindowTarget) -> Value {
    let Some(owner) = request.owner.clone() else {
        return failure(
            "observe",
            "invalid_ax_request",
            "AX observation needs a session owner",
        );
    };
    let (tx, rx) = mpsc::channel();
    if worker()
        .send(Job::Observe(owner, target, request.delivery_policy, tx))
        .is_err()
    {
        return failure(
            "observe",
            "ax_unavailable",
            "Accessibility worker is unavailable",
        );
    }
    rx.recv()
        .unwrap_or_else(|_| failure("observe", "ax_unavailable", "Accessibility worker stopped"))
}

pub(super) fn act(request: &ExecuteRequest, target: WindowTarget) -> Value {
    let Some(owner) = request.owner.clone() else {
        return failure(
            &request.action.action,
            "invalid_ax_request",
            "Element input needs a session owner",
        );
    };
    let (tx, rx) = mpsc::channel();
    if worker()
        .send(Job::Act(
            owner,
            target,
            request.delivery_policy,
            request.action.clone(),
            tx,
        ))
        .is_err()
    {
        return failure(
            &request.action.action,
            "ax_unavailable",
            "Accessibility worker is unavailable",
        );
    }
    let mut result = rx.recv().unwrap_or_else(|_| {
        let mut result = failure(
            &request.action.action,
            "ax_dispatch_uncertain",
            "Accessibility worker stopped; input may have arrived",
        );
        result["action_dispatched"] = Value::Null;
        result["retry_safe"] = json!(false);
        result
    });
    if request.action.include_screenshot == Some(true) {
        match mac_capture_window_group(target).and_then(encode_screenshot_capture) {
            Ok(frame) => {
                result["screenshot"] = frame;
                result["observation_kind"] = json!("ax_and_screenshot");
            }
            Err(reason) => result["screenshot_error"] = json!(reason),
        }
    }
    result
}

fn failure(action: &str, code: &str, reason: &str) -> Value {
    json!({"ok": false, "action": action, "error_code": code, "error": reason, "summary": reason,
        "action_dispatched": false, "dispatch_succeeded": false, "effect_verified": false,
        "retry_safe": true, "requires_observation": true, "input_method": "none"})
}

// Bound individual IPC requests as well as the complete walk. A hung app must
// not monopolize the serialized host input queue.
fn limit(element: &CFType) {
    unsafe {
        AXUIElementSetMessagingTimeout(element.as_CFTypeRef(), 0.15);
    }
}

fn read(element: &CFType, name: &str) -> Option<CFType> {
    limit(element);
    let name = CFString::new(name);
    let mut value = std::ptr::null();
    let status = unsafe {
        AXUIElementCopyAttributeValue(
            element.as_CFTypeRef(),
            name.as_concrete_TypeRef(),
            &mut value,
        )
    };
    if status != AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }
    Some(unsafe { CFType::wrap_under_create_rule(value) })
}

fn string(element: &CFType, name: &str) -> Option<String> {
    read(element, name)?
        .downcast::<CFString>()
        .map(|v| v.to_string())
}

fn boolean(element: &CFType, name: &str) -> Option<bool> {
    read(element, name)?.downcast::<CFBoolean>().map(bool::from)
}

fn array(element: &CFType, name: &str, count: usize) -> Vec<CFType> {
    limit(element);
    let name = CFString::new(name);
    let mut values = std::ptr::null();
    let status = unsafe {
        AXUIElementCopyAttributeValues(
            element.as_CFTypeRef(),
            name.as_concrete_TypeRef(),
            0,
            count as isize,
            &mut values,
        )
    };
    if status != AX_ERROR_SUCCESS || values.is_null() {
        return Vec::new();
    }
    let values = unsafe { CFArray::<CFType>::wrap_under_create_rule(values) };
    values.iter().map(|v| (*v).clone()).collect()
}

fn children(element: &CFType) -> Vec<CFType> {
    // Sheets precede large document contents. Retain this ordering when resolving.
    let mut values = array(element, "AXSheets", 32);
    for child in array(element, "AXChildren", MAX_VISITED + 1) {
        if !values.contains(&child) {
            values.push(child);
        }
    }
    values
}

fn actions(element: &CFType) -> Vec<String> {
    limit(element);
    let mut names = std::ptr::null();
    if unsafe { AXUIElementCopyActionNames(element.as_CFTypeRef(), &mut names) } != AX_ERROR_SUCCESS
        || names.is_null()
    {
        return Vec::new();
    }
    let names = unsafe { CFArray::<CFString>::wrap_under_create_rule(names) };
    names.iter().take(32).map(|name| name.to_string()).collect()
}

fn value_settable(element: &CFType) -> bool {
    limit(element);
    let mut settable = false;
    let name = CFString::new("AXValue");
    unsafe {
        AXUIElementIsAttributeSettable(
            element.as_CFTypeRef(),
            name.as_concrete_TypeRef(),
            &mut settable,
        ) == AX_ERROR_SUCCESS
            && settable
    }
}

fn secure(role: &str, subrole: Option<&str>) -> bool {
    role == "AXSecureTextField" || subrole == Some("AXSecureTextField")
}

fn text_role(role: &str) -> bool {
    matches!(
        role,
        "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField"
    )
}

fn value(element: &CFType) -> Value {
    let Some(value) = read(element, "AXValue") else {
        return Value::Null;
    };
    if let Some(text) = value.downcast::<CFString>() {
        return json!(text.to_string());
    }
    if let Some(boolean) = value.downcast::<CFBoolean>() {
        return json!(bool::from(boolean));
    }
    if let Some(number) = value.downcast::<CFNumber>() {
        return json!(number.to_f64());
    }
    Value::Null
}

fn signature(element: &CFType, protected: bool) -> Option<Value> {
    let role = string(element, "AXRole")?;
    let subrole = string(element, "AXSubrole");
    let protected = protected || secure(&role, subrole.as_deref());
    Some(json!({"role": role, "subrole": subrole,
        "identifier": string(element, "AXIdentifier"), "title": string(element, "AXTitle"),
        "description": if protected { None } else { string(element, "AXDescription") },
        "value": if protected { Value::Null } else { value(element) },
        "enabled": boolean(element, "AXEnabled"), "hidden": boolean(element, "AXHidden"),
        "selected": boolean(element, "AXSelected"), "protected": protected}))
}

fn fingerprint(value: &Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    hasher.finish()
}

fn expose_node(signature: &Value, actions: &[String], settable: bool, root: bool) -> bool {
    if root || settable || signature["protected"] == true {
        return true;
    }
    // Chromium exposes many anonymous layout wrappers with generic menu/scroll
    // actions. They must not consume the output budget ahead of document text.
    let structural = matches!(
        signature["role"].as_str(),
        Some("AXGroup" | "AXImage" | "AXStaticText" | "AXUnknown")
    );
    let has_text = ["title", "description", "value"].iter().any(|field| {
        let value = &signature[*field];
        !value.is_null() && value.as_str().is_none_or(|text| !text.trim().is_empty())
    });
    !structural
        || has_text
        || actions
            .iter()
            .any(|action| !matches!(action.as_str(), "AXShowMenu" | "AXScrollToVisible"))
}

fn clip(value: &mut Value, remaining: &mut usize, truncated: &mut bool) {
    match value {
        Value::String(text) => {
            let budget = (*remaining).min(2048);
            let clipped: String = text.chars().take(budget).collect();
            *remaining = remaining.saturating_sub(clipped.chars().count());
            if clipped.len() < text.len() {
                *truncated = true;
            }
            *text = clipped;
        }
        Value::Array(values) => {
            for value in values {
                clip(value, remaining, truncated);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                clip(value, remaining, truncated);
            }
        }
        _ => {}
    }
}

fn roots(
    application: &CFType,
    target: WindowTarget,
    relations: &WindowRelations,
) -> Result<Vec<CFType>, String> {
    let mut result = vec![mac_ax_window(application, target)?];
    for window in array(application, "AXWindows", 64) {
        if mac_ax_element_in_target(&window, target, relations) && !result.contains(&window) {
            result.push(window);
        }
    }
    Ok(result)
}

struct Node {
    element: CFType,
    path: Vec<usize>,
    identity: Value,
    fingerprint: u64,
    actions: Vec<String>,
    settable: bool,
}

struct Snapshot {
    id: String,
    created: Instant,
    target: WindowTarget,
    nodes: HashMap<String, Node>,
    observation: Value,
    // Chromium's remote AX connection needs a serviced observer run loop for
    // subsequent DOM changes to reach the accessibility tree reliably.
    _accessibility: Option<background_input::AccessibilityLease>,
}

#[derive(Default)]
struct Store {
    snapshots: HashMap<AxOwner, Snapshot>,
    sequence: u64,
}

impl Store {
    fn observe(&mut self, owner: AxOwner, target: WindowTarget, policy: DeliveryPolicy) -> Value {
        self.snapshots.remove(&owner);
        if let Err(reason) = require_permission() {
            return failure("observe", "ax_permission_required", &reason);
        }
        let snapshot = match self.capture(target, policy) {
            Ok(snapshot) => snapshot,
            Err(reason) => return failure("observe", "ax_unavailable", &reason),
        };
        if snapshot.nodes.len() <= 1 {
            return failure(
                "observe",
                "ax_empty",
                "The window exposes no useful accessibility content; request a screenshot",
            );
        }
        let result = json!({"ok": true, "action": "observe", "summary": "Observed application accessibility tree",
            "observation_kind": "ax", "accessibility": snapshot.observation,
            "action_dispatched": false, "effect_verified": false, "focus_isolation": "preserved"});
        if self.snapshots.len() >= MAX_OWNERS {
            if let Some(oldest) = self
                .snapshots
                .iter()
                .min_by_key(|(_, s)| s.created)
                .map(|(k, _)| k.clone())
            {
                self.snapshots.remove(&oldest);
            }
        }
        self.snapshots.insert(owner, snapshot);
        result
    }

    fn capture(
        &mut self,
        target: WindowTarget,
        policy: DeliveryPolicy,
    ) -> Result<Snapshot, String> {
        let accessibility = background_input::AccessibilityLease::start(target.pid).ok();
        let application = mac_ax_relations::application(target.pid)?;
        background_input::enable_accessibility(&application);
        let relations = mac_ax_relations::snapshot(&application, target.pid).relations;
        let roots = roots(&application, target, &relations)?;
        let focused = read(&application, "AXFocusedUIElement");
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut pending: VecDeque<_> = roots
            .into_iter()
            .enumerate()
            .map(|(index, root)| (root, vec![index], None::<String>, false))
            .collect();
        let rules = if policy == DeliveryPolicy::StrictBackground {
            background_input::ax_rules(target)
        } else {
            Vec::new()
        };
        let mut nodes = HashMap::new();
        let mut elements = Vec::new();
        let mut visited = Vec::new();
        let mut truncated = false;
        let mut text_remaining = MAX_TEXT;
        while let Some((element, path, parent, protected)) = pending.pop_front() {
            if nodes.len() >= MAX_NODES
                || visited.len() >= MAX_VISITED
                || Instant::now() >= deadline
                || text_remaining == 0
            {
                truncated = true;
                break;
            }
            if path.len() > 64 {
                truncated = true;
                continue;
            }
            if visited.contains(&element) {
                continue;
            }
            visited.push(element.clone());
            if !mac_ax_element_in_target(&element, target, &relations) {
                continue;
            }
            let Some(sig) = signature(&element, protected) else {
                continue;
            };
            if sig["hidden"] == true {
                continue;
            }
            let protected = sig["protected"] == true;
            let fingerprint = fingerprint(&sig);
            let identity = json!({"role": sig["role"], "subrole": sig["subrole"], "enabled": sig["enabled"], "protected": protected});
            let mut advertised = actions(&element);
            if protected {
                advertised.clear();
            }
            let role = sig["role"].as_str().unwrap_or("");
            let subrole = sig["subrole"].as_str();
            let settable = !protected && text_role(role) && value_settable(&element);
            let exposed = expose_node(&sig, &advertised, settable, path.len() == 1);
            let id = format!("e{}", elements.len() + 1);
            let child_parent = if exposed {
                Some(id.clone())
            } else {
                parent.clone()
            };
            // Depth-first traversal preserves reading order. Collapsing layout
            // wrappers changes public parents, never the native validation path.
            for (index, child) in children(&element).into_iter().enumerate().rev() {
                let mut child_path = path.clone();
                child_path.push(index);
                pending.push_front((child, child_path, child_parent.clone(), protected));
            }
            if !exposed {
                continue;
            }
            let enabled = sig["enabled"] != false;
            let allowed: Vec<_> = advertised
                .iter()
                .filter(|action| {
                    enabled
                        && (policy == DeliveryPolicy::AllowForeground
                            || background_input::ax_allowed(&rules, role, subrole, action))
                })
                .cloned()
                .collect();
            let allow_value = enabled
                && settable
                && (policy == DeliveryPolicy::AllowForeground
                    || background_input::ax_allowed(&rules, role, subrole, "AXValue"));
            let mut public = sig.clone();
            // Only user-visible text consumes the text budget, not role names,
            // identifiers and repeated protocol fields.
            for field in ["title", "description", "value"] {
                if let Some(value) = public.get_mut(field) {
                    clip(value, &mut text_remaining, &mut truncated);
                }
            }
            public["element_id"] = json!(id);
            public["parent_id"] = json!(parent);
            public["actions"] = json!(advertised);
            public["allowed_actions"] = json!(allowed);
            public["value_settable"] = json!(settable);
            public["allow_set_value"] = json!(allow_value);
            public["focused"] = json!(focused.as_ref() == Some(&element));
            // AX geometry is informative only: it can be zoomed relative to pixel
            // coordinates. Coordinate fallback always requires a fresh screenshot.
            public["ax_frame"] = mac_ax_frame(&element).map(|r| json!({"x": r.origin.x, "y": r.origin.y, "width": r.size.width, "height": r.size.height})).unwrap_or(Value::Null);
            if let Some(fields) = public.as_object_mut() {
                fields.retain(|_, value| !value.is_null());
            }
            elements.push(public);
            nodes.insert(
                id,
                Node {
                    element,
                    path,
                    identity,
                    fingerprint,
                    actions: advertised,
                    settable,
                },
            );
        }
        self.sequence += 1;
        let id = format!(
            "ax-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            self.sequence
        );
        let observation = json!({"snapshot_id": id, "window_id": target.window_id.to_string(), "elements": elements,
            "truncated": truncated, "max_nodes": MAX_NODES, "visited_nodes": visited.len(),
            "expires_in_ms": SNAPSHOT_TTL.as_millis(),
            "coordinate_space": "ax_screen_units_not_screenshot_pixels",
            "strict_background_ax_available": !rules.is_empty()});
        Ok(Snapshot {
            id,
            created: Instant::now(),
            target,
            nodes,
            observation,
            _accessibility: accessibility,
        })
    }

    fn act(
        &mut self,
        owner: AxOwner,
        target: WindowTarget,
        policy: DeliveryPolicy,
        action: ComputerAction,
    ) -> Value {
        // Consume references before any input; even failed or uncertain dispatch
        // cannot accidentally be replayed by a later call using the old snapshot.
        let Some(snapshot) = self.snapshots.remove(&owner) else {
            return failure(
                &action.action,
                "ax_reference_stale",
                "No current AX snapshot for this session; observe the window again",
            );
        };
        let stale = || {
            failure(
                &action.action,
                "ax_reference_stale",
                "The AX reference changed or expired; observe the window again",
            )
        };
        if snapshot.created.elapsed() >= SNAPSHOT_TTL
            || action.snapshot_id.as_deref() != Some(&snapshot.id)
            || snapshot.target.window_id != target.window_id
            || snapshot.target.pid != target.pid
        {
            return stale();
        }
        let Some(node) = action
            .element_id
            .as_ref()
            .and_then(|id| snapshot.nodes.get(id))
        else {
            return stale();
        };
        if let Err(reason) = require_permission() {
            return failure(&action.action, "ax_permission_required", &reason);
        }
        let application = match mac_ax_relations::application(target.pid) {
            Ok(app) => app,
            Err(_) => return stale(),
        };
        let relations = mac_ax_relations::snapshot(&application, target.pid).relations;
        let root_nodes = match roots(&application, target, &relations) {
            Ok(roots) => roots,
            Err(_) => return stale(),
        };
        let Some(mut current) = root_nodes.get(node.path[0]).cloned() else {
            return stale();
        };
        for &index in &node.path[1..] {
            let Some(child) = children(&current).get(index).cloned() else {
                return stale();
            };
            current = child;
        }
        if current != node.element
            || !mac_ax_element_in_target(&current, target, &relations)
            || signature(&current, node.identity["protected"] == true)
                .as_ref()
                .map(fingerprint)
                != Some(node.fingerprint)
        {
            return stale();
        }
        if node.identity["enabled"] == false || node.identity["protected"] == true {
            return failure(
                &action.action,
                "ax_action_unsupported",
                "Disabled and protected elements cannot receive semantic input",
            );
        }
        let operation = match action.action.as_str() {
            "press" => match ["AXPress", "AXPick"].into_iter().find(|name| node.actions.iter().any(|a| a == name)) {
                Some(name) => name,
                None => return failure(&action.action, "ax_action_unsupported", "This element exposes no press action; observe a screenshot for coordinate input"),
            },
            "perform_action" => match action.ax_action.as_deref().filter(|name| node.actions.iter().any(|a| a == name)) {
                Some(name) => name,
                None => return failure(&action.action, "ax_action_unsupported", "The requested AX action was not advertised by this element"),
            },
            "set_value" if node.settable => "AXValue",
            _ => return failure(&action.action, "ax_action_unsupported", "This element does not expose a writable text value"),
        };
        if operation != "AXValue" && !actions(&current).iter().any(|a| a == operation)
            || operation == "AXValue" && !value_settable(&current)
        {
            return failure(
                &action.action,
                "ax_action_unsupported",
                "The element's supported operations changed; observe again",
            );
        }
        let mut isolation = if policy == DeliveryPolicy::StrictBackground {
            let rules = background_input::ax_rules(target);
            if !background_input::ax_allowed(
                &rules,
                node.identity["role"].as_str().unwrap_or(""),
                node.identity["subrole"].as_str(),
                operation,
            ) {
                return failure(&action.action, "background_delivery_unsupported", "This OS/app/element/action has not passed strict-background AX validation; no input was sent");
            }
            match background_input::AxIsolation::start(target) {
                Ok(guard) => Some(guard),
                Err(reason) => {
                    return failure(&action.action, "background_delivery_unsupported", &reason)
                }
            }
        } else {
            None
        };
        if let Some(guard) = &isolation {
            if let Err(reason) = guard.check() {
                return failure(&action.action, "background_isolation_interrupted", &reason);
            }
        }
        // Starting observation hooks can wait for the app's run loop. Recheck
        // the selected control after that wait, immediately before dispatch.
        if signature(&current, false).as_ref().map(fingerprint) != Some(node.fingerprint) {
            return stale();
        }
        let foreground = MacForegroundMonitor::start(target.pid);
        let operation_name = CFString::new(operation);
        // Action handlers can legitimately take longer than an attribute read
        // (AppKit animations, renderer IPC, modal work). Never retry a timeout.
        unsafe {
            AXUIElementSetMessagingTimeout(current.as_CFTypeRef(), 1.0);
        }
        let status = if operation == "AXValue" {
            let text = CFString::new(action.text.as_deref().unwrap_or(""));
            unsafe {
                AXUIElementSetAttributeValue(
                    current.as_CFTypeRef(),
                    operation_name.as_concrete_TypeRef(),
                    text.as_CFTypeRef(),
                )
            }
        } else {
            unsafe {
                AXUIElementPerformAction(
                    current.as_CFTypeRef(),
                    operation_name.as_concrete_TypeRef(),
                )
            }
        };
        let mut result = dispatch_result(&action.action, operation, status);
        // An AX acknowledgement can precede renderer/AppKit state propagation.
        // Wait for observation only; no input is repeated during this settle.
        let value_deadline = Instant::now() + Duration::from_millis(500);
        let verified_value = if operation == "AXValue" && status == AX_ERROR_SUCCESS {
            loop {
                if string(&current, "AXValue").as_deref() == action.text.as_deref() {
                    break true;
                }
                if Instant::now() >= value_deadline {
                    break false;
                }
                thread::sleep(Duration::from_millis(25));
            }
        } else {
            false
        };
        thread::sleep(Duration::from_millis(CLICK_SETTLE_MS));
        let after = self.observe(owner, target, policy);
        if verified_value && string(&current, "AXValue").as_deref() == action.text.as_deref() {
            result["effect_verified"] = json!(true);
            result["verification_method"] = json!("ax_value");
        }
        if after["ok"] == true {
            result["ax_change_detected"] =
                json!(snapshot.observation["elements"] != after["accessibility"]["elements"]);
            result["accessibility"] = after["accessibility"].clone();
            result["observation_kind"] = json!("ax");
        } else {
            result["ax_error"] = after["error"].clone();
            result["requires_observation"] = json!(true);
        }
        if result["effect_verified"] != true && status == AX_ERROR_SUCCESS {
            result["verification_warning"] = json!("AX acknowledged the operation; its intended effect is unverified. Check the returned state or request a screenshot. Do not automatically repeat it, even if other AX attributes changed.");
        }
        if let Some(guard) = &mut isolation {
            guard.finish(&mut result);
        }
        record_click_foreground_change(
            &mut result,
            foreground.and_then(MacForegroundMonitor::finish),
        );
        result
    }
}

fn dispatch_result(action: &str, operation: &str, status: AXError) -> Value {
    let acknowledged = status == AX_ERROR_SUCCESS;
    let unsupported = matches!(status, -25202 | -25205 | -25206); // invalid element/unsupported attribute/action
    let mut result = json!({"ok": acknowledged, "action": action,
        "summary": if acknowledged { "Accessibility operation dispatched; inspect the returned state" } else { "Accessibility operation failed; inspect the receipt before continuing" },
        "action_dispatched": if acknowledged { Some(true) } else if unsupported { Some(false) } else { None },
        "dispatch_succeeded": acknowledged, "effect_verified": false, "retry_safe": unsupported,
        "requires_observation": !acknowledged,
        "input_method": if operation == "AXValue" { "accessibility_value" } else { "accessibility_action" }, "accessibility_action": operation});
    if !acknowledged {
        result["error_code"] = json!(if unsupported {
            "ax_action_unsupported"
        } else {
            "ax_dispatch_uncertain"
        });
        result["error"] = json!(mac_ax_error(operation, status));
    }
    result
}

#[cfg(test)]
mod tests;
