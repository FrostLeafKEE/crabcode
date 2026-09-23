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
    // The strict provider separately validates each OS/app/action combination.
    // Target-local input preparation never grants global foreground permission.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_policy_allows_window_targeted_input_but_blocks_focus() {
        for action in [
            "move",
            "click",
            "double_click",
            "drag",
            "scroll",
            "type",
            "keypress",
            "open_app",
        ] {
            assert!(
                rejection(
                    TargetScope::AppWindow,
                    DeliveryPolicy::StrictBackground,
                    action
                )
                .is_none(),
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
        assert!(rejection(
            TargetScope::AppWindow,
            DeliveryPolicy::StrictBackground,
            "focus_window"
        )
        .is_some());
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
