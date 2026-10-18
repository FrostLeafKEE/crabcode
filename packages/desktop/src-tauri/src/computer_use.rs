//! Thin Desktop transport adapter. Native execution belongs to the shared host.
// The isolated VM guest still links the exact same engine directly.
pub use crabcode_computer_use_core::{detect_capabilities, execute, ExecuteRequest};
use crabcode_computer_use_host as host;
use serde_json::{json, Value};
use std::path::PathBuf;
use tauri::Manager;

fn bundled_executable(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    let relative = "Crab Computer Use.app/Contents/MacOS/crabcode-computer-use-host";
    #[cfg(target_os = "windows")]
    let relative = "crabcode-computer-use-host.exe";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let relative = "crabcode-computer-use-host";
    let resources = app.path().resource_dir().map_err(|e| e.to_string())?;
    let bundled = resources.join("resources/computer-use").join(relative);
    if bundled.is_file() {
        return Ok(bundled);
    }
    #[cfg(debug_assertions)]
    {
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/computer-use")
            .join(relative);
        if development.is_file() {
            return Ok(development);
        }
    }
    Err("Computer Use component is missing. Reinstall Crab Desktop.".into())
}

async fn call(
    app: tauri::AppHandle,
    method: &'static str,
    params: Value,
    start: bool,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = host::state_dir()?;
        let endpoint = if start {
            let executable = host::install(&bundled_executable(&app)?, &dir)?;
            host::ensure_running(&dir, &executable)?
        } else {
            host::read_endpoint(&dir)?
        };
        host::rpc(&endpoint, method, params)
    })
    .await
    .map_err(|e| format!("Computer Use worker failed: {e}"))?
}

#[tauri::command]
pub async fn computer_use_capabilities(app: tauri::AppHandle) -> Result<Value, String> {
    call(app, "capabilities", json!({}), true).await
}

#[tauri::command]
pub async fn computer_use_open_input_settings(app: tauri::AppHandle, kind: Option<String>) -> Result<(), String> {
    call(app, "permissions", json!({"kind": kind}), true).await.map(|_| ())
}

#[tauri::command]
pub async fn computer_use_release_ax(
    app: tauri::AppHandle,
    host_id: String,
    connection_id: String,
    session_id: Option<String>,
    agent_id: Option<String>,
    all_agents: bool,
) -> Result<(), String> {
    call(
        app,
        "release",
        json!({"hostId": host_id, "connectionId": connection_id, "sessionId": session_id,
        "agentId": agent_id, "allAgents": all_agents}),
        false,
    )
    .await
    .map(|_| ())
}

#[tauri::command]
pub async fn computer_use_heartbeat(
    app: tauri::AppHandle,
    host_id: String,
    connection_id: String,
    instance_id: String,
) -> Result<(), String> {
    let endpoint = host::read_endpoint(&host::state_dir()?)?;
    if endpoint.instance_id != instance_id {
        return Err("Computer Use executor restarted".into());
    }
    call(
        app,
        "heartbeat",
        json!({"hostId":host_id,"connectionId":connection_id}),
        false,
    )
    .await
    .map(|_| ())
}

#[tauri::command]
pub async fn computer_use_execute(
    app: tauri::AppHandle,
    request: Value,
    instance_id: String,
    lease: Value,
) -> Result<Value, String> {
    // Never start a replacement host or replay an input after a transport error.
    call(
        app,
        "execute",
        json!({"request":request,"instanceId":instance_id,"lease":lease}),
        false,
    )
    .await
}
