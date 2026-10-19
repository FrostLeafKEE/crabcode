"""Resolve the implicit target against the connected executor, not the Gateway OS."""

from typing import Any


def effective_computer_use_mode(session: Any) -> str:
    """Adapt only the omitted default; explicit targets and permissions never change."""
    mode = getattr(session, "computer_use_mode", "background_app")
    if mode not in ("background_app", "foreground_desktop"):
        mode = "background_app"
    configured = getattr(getattr(session, "settings", None), "computer_use", None)
    if (
        mode != "background_app"
        or configured is None
        or getattr(session, "_computer_use_mode_override", None) is not None
        or "target_scope" in getattr(configured, "model_fields_set", {"target_scope"})
        or getattr(
            session, "effective_computer_use_delivery_policy",
            getattr(session, "computer_use_delivery_policy", "allow_foreground"),
        ) != "allow_foreground"
    ):
        return mode
    backend = getattr(session, "computer_use_backend", None)
    capabilities = getattr(backend, "capabilities", None)
    caps = (
        capabilities(getattr(session, "computer_use_host_id", None))
        if callable(capabilities) else {}
    )
    modes = caps.get("supported_modes") if isinstance(caps, dict) else None
    if (
        isinstance(caps, dict)
        and caps.get("platform") in ("windows", "win32")
        and isinstance(modes, list)
        and "foreground_desktop" in modes
        and "background_app" not in modes
    ):
        return "foreground_desktop"
    return mode
