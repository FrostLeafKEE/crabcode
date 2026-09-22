//! Host-enforced input policy. Target routing never grants foreground permission.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum TargetScope {
    AppWindow,
    Desktop,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum DeliveryPolicy {
    StrictBackground,
    #[default]
    AllowForeground,
}

pub(super) fn observation_only(action: &str) -> bool {
    matches!(
        action,
        "observe" | "list_windows" | "list_displays" | "wait"
    )
}

pub(super) fn rejection(
    scope: TargetScope,
    policy: DeliveryPolicy,
    action: &str,
) -> Option<&'static str> {
    if policy == DeliveryPolicy::AllowForeground {
        return None;
    }
    if scope == TargetScope::Desktop {
        return Some("Desktop scope requires allow_foreground");
    }
    if observation_only(action) {
        return None;
    }
    if action == "focus_window" {
        return Some("focus_window requires allow_foreground");
    }
    // AXPress itself can activate an app; PID routing and a retrospective
    // foreground poll do not prevent that. Until a preventive focus provider
    // has passed input-isolation tests, refuse BEFORE even enabling AX or
    // launching an app. Runtime symbol availability alone is insufficient.
    Some("Strict background input is unavailable: no verified preventive focus provider. No action was dispatched.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_policy_blocks_every_input_route_before_dispatch() {
        for action in [
            "move",
            "click",
            "double_click",
            "drag",
            "scroll",
            "type",
            "keypress",
            "focus_window",
            "open_app",
        ] {
            assert!(
                rejection(
                    TargetScope::AppWindow,
                    DeliveryPolicy::StrictBackground,
                    action
                )
                .is_some(),
                "{action}"
            );
            assert!(
                rejection(
                    TargetScope::AppWindow,
                    DeliveryPolicy::AllowForeground,
                    action
                )
                .is_none(),
                "{action}"
            );
        }
        for action in ["observe", "list_windows", "wait"] {
            assert!(rejection(
                TargetScope::AppWindow,
                DeliveryPolicy::StrictBackground,
                action
            )
            .is_none());
        }
        assert!(rejection(
            TargetScope::Desktop,
            DeliveryPolicy::StrictBackground,
            "observe"
        )
        .is_some());
    }
}
