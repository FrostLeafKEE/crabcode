//! Read-only AX relationship collection. Missing AX data never implies ownership.
use super::*;

pub(super) struct AxWindowSnapshot {
    pub relations: WindowRelations,
    pub focused_window_id: Option<u32>,
    pub focused_element_role: Option<String>,
}

fn array(element: &CFType, attribute: &str) -> Vec<CFType> {
    mac_ax_copy_attribute(element, attribute)
        .ok()
        .and_then(|value| value.downcast::<CFArray>())
        .map(|values| {
            values
                .get_all_values()
                .into_iter()
                .filter(|value| !value.is_null())
                .map(|value| unsafe { CFType::wrap_under_get_rule(value.cast()) })
                .collect()
        })
        .unwrap_or_default()
}

fn kind(element: &CFType) -> WindowKind {
    match mac_ax_string(element, "AXRole").as_deref() {
        Some("AXSheet") => WindowKind::Sheet,
        Some("AXPopover") => WindowKind::Popover,
        Some("AXWindow") => match mac_ax_string(element, "AXSubrole").as_deref() {
            Some("AXStandardWindow") => WindowKind::Document,
            Some("AXDialog" | "AXSystemDialog" | "AXFloatingWindow") => WindowKind::Auxiliary,
            _ => WindowKind::Unknown,
        },
        // A missing action/role is not evidence of passivity. Only an explicitly
        // disabled image surface may be passed through by pointer routing.
        Some("AXImage")
            if mac_ax_copy_attribute(element, "AXWindow").ok().as_ref() == Some(element)
                && mac_ax_copy_attribute(element, "AXEnabled")
                    .ok()
                    .and_then(|value| value.downcast::<CFBoolean>())
                    .map(bool::from)
                    == Some(false) =>
        {
            WindowKind::Passive
        }
        _ => WindowKind::Unknown,
    }
}

fn belongs_to_process(element: &CFType, pid: i32) -> bool {
    let mut actual = 0;
    unsafe {
        AXUIElementGetPid(element.as_CFTypeRef(), &mut actual) == AX_ERROR_SUCCESS && actual == pid
    }
}

fn link(relations: &mut WindowRelations, child: u32, owner: u32, evidence: &str) {
    if child == owner {
        return;
    }
    let kind = match relations.kind(child) {
        WindowKind::Unknown => WindowKind::Auxiliary,
        kind => kind,
    };
    relations.record(child, kind, Some((owner, evidence)));
}

// AXParent and AXWindow are ownership edges; AXFocusedWindow, AXMainWindow,
// AXTopLevelUIElement and a shared application container are not owner edges.
fn ancestry(element: &CFType, pid: i32, relations: &mut WindowRelations) -> Option<u32> {
    let mut current = element.clone();
    let mut visited = Vec::new();
    let mut nearest = None;
    let mut previous = None;
    for _ in 0..32 {
        if !belongs_to_process(&current, pid)
            || visited.contains(&current)
            || mac_ax_string(&current, "AXRole").as_deref() == Some("AXApplication")
        {
            break;
        }
        visited.push(current.clone());
        if let Ok(id) = mac_ax_window_id(&current) {
            nearest.get_or_insert(id);
            relations.record(id, kind(&current), None);
            if let Some(child) = previous {
                link(relations, child, id, "AXParent");
            }
            previous = Some(id);
            if let Ok(window) = mac_ax_copy_attribute(&current, "AXWindow") {
                if belongs_to_process(&window, pid) {
                    if let Ok(owner) = mac_ax_window_id(&window) {
                        relations.record(owner, kind(&window), None);
                        link(relations, id, owner, "AXWindow");
                    }
                }
            }
        }
        let Ok(parent) = mac_ax_copy_attribute(&current, "AXParent") else {
            break;
        };
        current = parent;
    }
    nearest
}

fn descendants(
    element: CFType,
    pid: i32,
    parent: Option<(u32, &'static str)>,
    relations: &mut WindowRelations,
    visited: &mut Vec<CFType>,
    depth: usize,
) {
    if depth > 16
        || visited.len() >= 512
        || visited.contains(&element)
        || !belongs_to_process(&element, pid)
    {
        return;
    }
    visited.push(element.clone());
    let id = mac_ax_window_id(&element).ok();
    if let Some(id) = id {
        relations.record(id, kind(&element), None);
        if let Some((owner, evidence)) = parent {
            link(relations, id, owner, evidence);
        }
    }
    // Stay at window/sheet boundaries instead of walking every text node in
    // every document. Editors deeper in the tree are inspected through the
    // focused responder's ancestry, independently of this traversal budget.
    if parent.is_some_and(|(owner, _)| id == Some(owner))
        && !matches!(
            mac_ax_string(&element, "AXRole").as_deref(),
            Some("AXWindow" | "AXSheet" | "AXPopover")
        )
    {
        return;
    }
    let owner = id.or(parent.map(|(id, _)| id));
    // Sheets first so a large document tree cannot exhaust the traversal budget.
    for attribute in ["AXSheets", "AXChildren"] {
        for child in array(&element, attribute) {
            descendants(
                child,
                pid,
                owner.map(|id| (id, attribute)),
                relations,
                visited,
                depth + 1,
            );
        }
    }
}

pub(super) fn snapshot(application: &CFType, pid: i32) -> AxWindowSnapshot {
    let mut relations = WindowRelations::default();
    let windows = array(application, "AXWindows");
    // Classify all top-level documents before traversing any child content.
    for window in &windows {
        if belongs_to_process(window, pid) {
            if let Ok(id) = mac_ax_window_id(window) {
                relations.record(id, kind(window), None);
            }
            ancestry(window, pid, &mut relations);
        }
    }
    for window in windows {
        descendants(window, pid, None, &mut relations, &mut Vec::new(), 0);
    }
    let mut focus = focus_snapshot(application, pid);
    for (id, relation) in relations.0 {
        focus.relations.merge(id, relation);
    }
    focus
}

pub(super) fn focus_snapshot(application: &CFType, pid: i32) -> AxWindowSnapshot {
    let mut relations = WindowRelations::default();
    let focused = match mac_ax_copy_attribute(application, "AXFocusedUIElement") {
        Ok(element) => Some(element),
        // Only absence/unsupported allows falling back to the app's focused
        // window. A failed AX message is uncertainty, not an absent responder.
        Err(-25212 | -25205) => mac_ax_copy_attribute(application, "AXFocusedWindow").ok(),
        Err(_) => None,
    };
    let focused_window_id = focused
        .as_ref()
        .and_then(|element| ancestry(element, pid, &mut relations));
    AxWindowSnapshot {
        relations,
        focused_window_id,
        focused_element_role: focused
            .as_ref()
            .and_then(|element| mac_ax_string(element, "AXRole")),
    }
}

pub(super) fn application(pid: i32) -> Result<CFType, String> {
    let raw = unsafe { AXUIElementCreateApplication(pid) };
    if raw.is_null() {
        return Err("Unable to inspect the target application's accessibility tree".into());
    }
    Ok(unsafe { CFType::wrap_under_create_rule(raw) })
}
