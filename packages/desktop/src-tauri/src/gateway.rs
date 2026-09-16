use crate::settings::read_credential;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read};
use std::net::{IpAddr, ToSocketAddrs};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use url::Url;

const MIN_PYTHON_MAJOR: u32 = 3;
const MIN_PYTHON_MINOR: u32 = 10;
const GATEWAY_PROTOCOL: i64 = 1;
const DESKTOP_ORIGIN: &str = "tauri://localhost";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn configure_python_utf8(command: &mut Command) {
    command
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8");
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
}

fn configure_gateway_command(command: &mut Command, host: &str, port: &str) {
    configure_python_utf8(command);
    command.args([
        "-m",
        "crabcode_cli",
        "gateway",
        "--host",
        host,
        "--port",
        port,
        "--cors",
        DESKTOP_ORIGIN,
    ]);
}

fn stop_child_tree(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let pid = child.id().to_string();
        let status = Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        if status.is_ok_and(|value| value.success()) {
            let _ = child.wait();
            return Ok(());
        }
    }

    if child.try_wait()?.is_none() {
        child.kill()?;
        let _ = child.wait();
    }
    Ok(())
}

#[derive(Default)]
pub struct GatewayProcesses {
    children: Mutex<HashMap<String, Child>>,
    startup: Mutex<()>,
}

impl GatewayProcesses {
    pub fn stop_all(&self) {
        if let Ok(mut processes) = self.children.lock() {
            for (_, mut child) in processes.drain() {
                let _ = stop_child_tree(&mut child);
            }
        }
    }
}

#[derive(Serialize)]
pub struct AuthResult {
    access_token: Option<String>,
    expires_in: u64,
    mode: String,
}

#[derive(Deserialize)]
struct AuthInfo {
    mode: String,
    methods: Vec<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Serialize)]
pub struct EnsureGatewayResult {
    ready: bool,
    started_by_desktop: bool,
    python: Option<String>,
    version: Option<String>,
    message: String,
}

fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| format!("Unable to create HTTP client: {error}"))
}

fn parse_base_url(value: &str) -> Result<Url, String> {
    let mut url = Url::parse(value).map_err(|error| format!("Invalid Gateway URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Gateway URL must use http:// or https://".to_string());
    }
    if url.host_str().is_none() {
        return Err("Gateway URL must include a host".to_string());
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let next = format!("{}/", url.path().trim_end_matches('/'));
        url.set_path(&next);
    }
    Ok(url)
}

fn endpoint(base: &Url, path: &str) -> Result<Url, String> {
    base.join(path.trim_start_matches('/'))
        .map_err(|error| format!("Unable to construct Gateway URL: {error}"))
}

#[tauri::command]
pub async fn authenticate_connection(
    base_url: String,
    credential_ref: Option<String>,
) -> Result<AuthResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        authenticate_connection_blocking(base_url, credential_ref)
    })
    .await
    .map_err(|error| format!("Gateway authentication task failed: {error}"))?
}

fn authenticate_connection_blocking(
    base_url: String,
    credential_ref: Option<String>,
) -> Result<AuthResult, String> {
    let base = parse_base_url(&base_url)?;
    let http = client()?;
    let info: AuthInfo = http
        .get(endpoint(&base, "auth/info")?)
        .send()
        .map_err(|error| format!("Unable to reach Gateway authentication endpoint: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Gateway authentication discovery failed: {error}"))?
        .json()
        .map_err(|error| format!("Gateway returned invalid authentication metadata: {error}"))?;

    if info.mode == "none" {
        return Ok(AuthResult {
            access_token: None,
            expires_in: 0,
            mode: info.mode,
        });
    }
    if !info.methods.iter().any(|method| method == "password") {
        return Err("This Gateway does not support password authentication".to_string());
    }
    let reference = credential_ref
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "This Gateway requires a saved password".to_string())?;
    let password = read_credential(&reference)?;
    let token: TokenResponse = http
        .post(endpoint(&base, "auth/token")?)
        .json(&json!({"grant_type": "password", "password": password}))
        .send()
        .map_err(|error| format!("Unable to authenticate with Gateway: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Gateway rejected the password: {error}"))?
        .json()
        .map_err(|error| format!("Gateway returned an invalid token response: {error}"))?;
    Ok(AuthResult {
        access_token: Some(token.access_token),
        expires_in: token.expires_in,
        mode: info.mode,
    })
}

fn is_loopback(base: &Url) -> bool {
    let Some(host) = base.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if host
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
    {
        return true;
    }
    let port = base.port_or_known_default().unwrap_or(80);
    (host, port)
        .to_socket_addrs()
        .map(|addresses| {
            addresses
                .into_iter()
                .all(|address| address.ip().is_loopback())
        })
        .unwrap_or(false)
}

fn probe_health(base: &Url, credential_ref: Option<&str>) -> Result<Option<Value>, String> {
    let http = client()?;
    let mut request = http.get(endpoint(base, "health")?);
    if let Some(reference) = credential_ref {
        if let Ok(password) = read_credential(reference) {
            request = request.bearer_auth(password);
        }
    }
    let response = match request.send() {
        Ok(response) => response,
        Err(error) if error.is_connect() || error.is_timeout() => return Ok(None),
        Err(error) => return Err(format!("Gateway health check failed: {error}")),
    };
    if response.status().as_u16() == 401 {
        return Ok(Some(
            json!({"status": "authenticated", "version": null, "protocol_version": GATEWAY_PROTOCOL}),
        ));
    }
    let response = response
        .error_for_status()
        .map_err(|error| format!("Gateway health check failed: {error}"))?;
    let value: Value = response.json().map_err(|error| {
        format!("The service at this address is not a CrabCode Gateway: {error}")
    })?;
    if value.get("status").and_then(Value::as_str).is_none()
        || value.get("version").and_then(Value::as_str).is_none()
    {
        return Err("The health endpoint is not a CrabCode Gateway".to_string());
    }
    let min = value
        .get("min_protocol_version")
        .and_then(Value::as_i64)
        .or_else(|| value.get("protocol_version").and_then(Value::as_i64));
    let max = value
        .get("max_protocol_version")
        .and_then(Value::as_i64)
        .or_else(|| value.get("protocol_version").and_then(Value::as_i64));
    if !matches!((min, max), (Some(low), Some(high)) if low <= GATEWAY_PROTOCOL && high >= GATEWAY_PROTOCOL)
    {
        return Err("The running Gateway does not support protocol v1".to_string());
    }
    Ok(Some(value))
}

fn python_version(candidate: &str) -> Option<(u32, u32)> {
    let mut command = Command::new(candidate);
    configure_python_utf8(&mut command);
    let output = command.arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };
    let version = raw.split_whitespace().find(|part| {
        part.chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
    })?;
    let mut parts = version.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn supported_gateway_python(candidate: &str) -> bool {
    python_version(candidate).is_some_and(|(major, minor)| {
        major > MIN_PYTHON_MAJOR || (major == MIN_PYTHON_MAJOR && minor >= MIN_PYTHON_MINOR)
    })
}

fn push_unique(candidates: &mut Vec<String>, candidate: String) {
    if !candidates.contains(&candidate) {
        candidates.push(candidate);
    }
}

#[cfg(target_os = "macos")]
fn push_path(candidates: &mut Vec<String>, path: &Path) {
    push_unique(candidates, path.to_string_lossy().into_owned());
}

#[cfg(target_os = "macos")]
fn extend_version_managed_pythons(candidates: &mut Vec<String>, root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bin/python3"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths.reverse();
    for path in paths {
        push_path(candidates, &path);
    }
}

#[cfg(target_os = "macos")]
fn extend_homebrew_pythons(candidates: &mut Vec<String>, root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_name().to_string_lossy().starts_with("python@") {
            continue;
        }
        for relative in ["libexec/bin/python3", "bin/python3"] {
            let path = entry.path().join(relative);
            if path.is_file() {
                push_path(candidates, &path);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn extend_macos_python_candidates(candidates: &mut Vec<String>, home: Option<&Path>) {
    for path in [
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
        "/opt/local/bin/python3",
        "/opt/anaconda3/bin/python3",
        "/opt/miniconda3/bin/python3",
        "/opt/miniforge3/bin/python3",
        "/Library/Frameworks/Python.framework/Versions/Current/bin/python3",
    ] {
        push_path(candidates, Path::new(path));
    }
    for root in [Path::new("/opt/homebrew/opt"), Path::new("/usr/local/opt")] {
        extend_homebrew_pythons(candidates, root);
    }
    if let Some(home) = home {
        for relative in [
            "anaconda3/bin/python3",
            "miniconda3/bin/python3",
            "miniforge3/bin/python3",
            "mambaforge/bin/python3",
            ".local/bin/python3",
            ".pyenv/shims/python3",
            ".asdf/shims/python3",
        ] {
            push_path(candidates, &home.join(relative));
        }
        for relative in [
            ".pyenv/versions",
            ".asdf/installs/python",
            ".local/share/uv/python",
        ] {
            extend_version_managed_pythons(candidates, &home.join(relative));
        }
    }
}

fn python_candidates(configured: Option<&str>, include_managed: bool) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(value) = configured.filter(|value| !value.trim().is_empty()) {
        push_unique(&mut candidates, value.to_string());
    }
    if include_managed {
        if let Ok(environment) = managed_gateway_environment_dir() {
            push_unique(
                &mut candidates,
                managed_gateway_python_path(&environment)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    push_unique(&mut candidates, "python3".to_string());
    push_unique(&mut candidates, "python".to_string());
    #[cfg(target_os = "windows")]
    push_unique(&mut candidates, "py".to_string());

    // Finder-launched macOS apps do not inherit the user's shell PATH. Check
    // common package-manager and version-manager locations explicitly so the
    // packaged app sees the same Python installations as a terminal session.
    #[cfg(target_os = "macos")]
    extend_macos_python_candidates(&mut candidates, dirs::home_dir().as_deref());
    candidates
}

#[cfg(not(debug_assertions))]
fn detect_python(configured: Option<&str>) -> Result<String, String> {
    for candidate in python_candidates(configured, true) {
        if supported_gateway_python(&candidate) {
            return Ok(candidate);
        }
    }
    Err("Python 3.10 or newer was not found. Install Python or set a Python path in Desktop settings.".to_string())
}

#[cfg(debug_assertions)]
fn detect_development_python(configured: Option<&str>) -> Result<String, String> {
    for candidate in python_candidates(configured, false) {
        if supported_gateway_python(&candidate) {
            return Ok(candidate);
        }
    }
    Err("Python 3.10 or newer was not found. Install Python or set a Python path in Desktop settings.".to_string())
}

fn detect_document_engine_python(configured: Option<&str>) -> Result<String, String> {
    for candidate in python_candidates(configured, true) {
        if python_version(&candidate)
            .is_some_and(|(major, minor)| major == 3 && (10..=13).contains(&minor))
        {
            return Ok(candidate);
        }
    }
    Err("The high-fidelity PDF engine requires Python 3.10 through 3.13.".to_string())
}

fn run_document_engine_command(
    python_path: Option<&str>,
    arguments: &[&str],
) -> Result<Value, String> {
    let python = detect_document_engine_python(python_path)?;
    let mut command = Command::new(&python);
    configure_python_utf8(&mut command);
    let output = command
        .args(["-m", "crabcode_cli", "document-engine"])
        .args(arguments)
        .output()
        .map_err(|error| format!("Unable to start document engine manager: {error}"))?;
    parse_document_engine_output(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )
}

fn parse_document_engine_output(
    succeeded: bool,
    stdout: &str,
    stderr: &str,
) -> Result<Value, String> {
    let parsed = serde_json::from_str::<Value>(stdout.trim()).ok();
    if succeeded {
        return parsed.ok_or_else(|| "Document engine manager returned invalid JSON.".to_string());
    }
    if let Some(detail) = parsed
        .as_ref()
        .and_then(|value| value.get("detail"))
        .and_then(Value::as_str)
    {
        return Err(detail.to_string());
    }
    let stderr = stderr.trim().to_string();
    Err(if stderr.is_empty() {
        "Document engine manager failed.".to_string()
    } else {
        stderr
    })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentEngineInstallProgress {
    operation_id: String,
    stage: String,
    detail: String,
    percent: u8,
}

fn install_progress(operation_id: &str, detail: &str) -> DocumentEngineInstallProgress {
    let (stage, percent) = if detail.contains("启用") {
        ("activating", 96)
    } else if detail.contains("下载并校验") {
        ("downloading_assets", 68)
    } else if detail.contains("校验") {
        ("verifying", 88)
    } else if detail.contains("安装程序与依赖") || detail.contains("正在安装") {
        ("installing", 36)
    } else if detail.contains("创建独立 Python 环境") {
        ("creating_environment", 15)
    } else {
        ("preparing", 5)
    };
    DocumentEngineInstallProgress {
        operation_id: operation_id.to_string(),
        stage: stage.to_string(),
        detail: detail.to_string(),
        percent,
    }
}

fn run_document_engine_install_command(
    app: AppHandle,
    operation_id: String,
    python_path: Option<String>,
    bundle: Option<String>,
) -> Result<Value, String> {
    let python = detect_document_engine_python(python_path.as_deref())?;
    let mut command = Command::new(&python);
    configure_python_utf8(&mut command);
    command
        .args(["-m", "crabcode_cli", "document-engine", "install", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = bundle.as_deref().filter(|value| !value.trim().is_empty()) {
        command.args(["--bundle", path]);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("Unable to start document engine manager: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Unable to read document engine manager output.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Unable to read document engine manager progress.".to_string())?;

    let stdout_thread = thread::spawn(move || {
        let mut output = String::new();
        let mut reader = BufReader::new(stdout);
        reader
            .read_to_string(&mut output)
            .map(|_| output)
            .map_err(|error| format!("Unable to read document engine manager output: {error}"))
    });
    let progress_app = app.clone();
    let progress_operation_id = operation_id.clone();
    let stderr_thread = thread::spawn(move || {
        let mut captured = Vec::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.map_err(|error| {
                format!("Unable to read document engine manager progress: {error}")
            })?;
            if line.starts_with("正在") {
                let _ = progress_app.emit(
                    "document-engine-install-progress",
                    install_progress(&progress_operation_id, &line),
                );
            }
            captured.push(line);
        }
        Ok::<String, String>(captured.join("\n"))
    });

    let status = child
        .wait()
        .map_err(|error| format!("Unable to wait for document engine manager: {error}"))?;
    let stdout = stdout_thread
        .join()
        .map_err(|_| "Document engine output reader stopped unexpectedly.".to_string())??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| "Document engine progress reader stopped unexpectedly.".to_string())??;
    let result = parse_document_engine_output(status.success(), &stdout, &stderr);
    if result.is_ok() {
        let _ = app.emit(
            "document-engine-install-progress",
            DocumentEngineInstallProgress {
                operation_id,
                stage: "complete".to_string(),
                detail: "高精度 PDF 引擎安装完成".to_string(),
                percent: 100,
            },
        );
    }
    result
}

#[tauri::command]
pub async fn document_engine_status(python_path: Option<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        run_document_engine_command(python_path.as_deref(), &["status", "--json"])
    })
    .await
    .map_err(|error| format!("Document engine status task failed: {error}"))?
}

#[tauri::command]
pub async fn install_document_engine(
    app: AppHandle,
    python_path: Option<String>,
    bundle: Option<String>,
    operation_id: String,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        run_document_engine_install_command(app, operation_id, python_path, bundle)
    })
    .await
    .map_err(|error| format!("Document engine installer task failed: {error}"))?
}

#[tauri::command]
pub async fn remove_document_engine(python_path: Option<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        run_document_engine_command(python_path.as_deref(), &["remove", "--yes", "--json"])
    })
    .await
    .map_err(|error| format!("Document engine removal task failed: {error}"))?
}

fn installed_gateway_version(python: &str) -> Option<String> {
    let script = "import crabcode_gateway; print(getattr(crabcode_gateway, '__version__', ''))";
    let mut command = Command::new(python);
    configure_python_utf8(&mut command);
    let output = command.args(["-c", script]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}

fn managed_gateway_environment_dir() -> Result<PathBuf, String> {
    let home =
        dirs::home_dir().ok_or_else(|| "Unable to locate the user home directory".to_string())?;
    Ok(home.join(".crabcode").join("desktop").join("gateway-venv"))
}

fn managed_gateway_python_path(environment: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    return environment.join("Scripts").join("python.exe");
    #[cfg(not(target_os = "windows"))]
    return environment.join("bin").join("python");
}

#[cfg(any(not(debug_assertions), test))]
fn ensure_managed_gateway_python_at(
    base_python: &str,
    environment: &Path,
    on_output: &(impl Fn(&str) + Sync),
) -> Result<String, String> {
    let managed_python = managed_gateway_python_path(environment);
    let managed_python_string = managed_python.to_string_lossy().into_owned();
    if supported_gateway_python(&managed_python_string) {
        return Ok(managed_python_string);
    }

    let parent = environment
        .parent()
        .ok_or_else(|| "Invalid managed Gateway environment path".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("Unable to create the Gateway environment directory: {error}"))?;
    let environment_string = environment.to_string_lossy().into_owned();
    let mut command = Command::new(base_python);
    configure_python_utf8(&mut command);
    command.args(["-m", "venv", "--clear", &environment_string]);
    let (status, detail) = run_streaming_command(&mut command, on_output)?;
    if !status.success() {
        return Err(format!(
            "Unable to create the managed Gateway environment with `{base_python} -m venv`. {detail}"
        ));
    }
    if !supported_gateway_python(&managed_python_string) {
        return Err(
            "The managed Gateway environment did not provide a working Python interpreter"
                .to_string(),
        );
    }
    Ok(managed_python_string)
}

#[cfg(not(debug_assertions))]
fn ensure_managed_gateway_python(
    base_python: &str,
    on_output: &(impl Fn(&str) + Sync),
) -> Result<String, String> {
    let environment = managed_gateway_environment_dir()?;
    ensure_managed_gateway_python_at(base_python, &environment, on_output)
}

#[derive(Deserialize)]
struct PythonEnvironment {
    version: String,
    executable: String,
    prefix: String,
    kind: String,
}

fn python_environment(python: &str) -> Option<PythonEnvironment> {
    let script = r#"
import json, platform, sys
from pathlib import Path
kind = "Conda 环境" if (Path(sys.prefix) / "conda-meta").is_dir() else "虚拟环境" if sys.prefix != sys.base_prefix else "基础环境"
print(json.dumps({"version": platform.python_version(), "executable": sys.executable, "prefix": sys.prefix, "kind": kind}))
"#;
    let mut command = Command::new(python);
    configure_python_utf8(&mut command);
    let output = command.args(["-c", script]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayStartupProgress {
    connection_id: String,
    operation_id: String,
    stage: String,
    detail: String,
}

// Drain both pipes concurrently so a verbose installer cannot deadlock. Keep only
// a bounded tail for failures; each line is delivered to the UI as it arrives.
fn run_streaming_command(
    command: &mut Command,
    on_output: &(impl Fn(&str) + Sync),
) -> Result<(ExitStatus, String), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Unable to start setup command: {error}"))?;
    let stdout = child.stdout.take().ok_or("Unable to read setup output")?;
    let stderr = child.stderr.take().ok_or("Unable to read setup errors")?;
    let read_output = |stream: &mut dyn Read| -> Result<String, String> {
        let mut tail = VecDeque::new();
        let mut reader = BufReader::new(stream);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            if reader
                .read_until(b'\n', &mut bytes)
                .map_err(|error| error.to_string())?
                == 0
            {
                break;
            }
            let line: String = String::from_utf8_lossy(&bytes)
                .trim()
                .chars()
                .take(4096)
                .collect();
            if line.is_empty() {
                continue;
            }
            on_output(&line);
            tail.push_back(line);
            if tail.len() > 40 {
                tail.pop_front();
            }
        }
        Ok(tail.into_iter().collect::<Vec<_>>().join("\n"))
    };
    thread::scope(|scope| {
        let stdout_reader = scope.spawn(|| read_output(&mut { stdout }));
        let stderr_reader = scope.spawn(|| read_output(&mut { stderr }));
        let status = child
            .wait()
            .map_err(|error| format!("Unable to wait for setup command: {error}"))?;
        let stdout = stdout_reader
            .join()
            .map_err(|_| "Setup output reader stopped unexpectedly")??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| "Setup error reader stopped unexpectedly")??;
        Ok((status, [stdout, stderr].join("\n").trim().to_string()))
    })
}

fn install_gateway(python: &str, on_output: &(impl Fn(&str) + Sync)) -> Result<(), String> {
    let package = format!("crabcode[gateway]=={}", env!("CARGO_PKG_VERSION"));
    let mut command = Command::new(python);
    configure_python_utf8(&mut command);
    command.args([
        "-u",
        "-m",
        "pip",
        "install",
        "--upgrade",
        "--no-input",
        "--progress-bar",
        "off",
        &package,
    ]);
    let (status, detail) = run_streaming_command(&mut command, on_output)?;
    if status.success() {
        return Ok(());
    }
    Err(format!(
        "Failed to install {package}. Run `{python} -m pip install --upgrade \"{package}\"` manually. {detail}"
    ))
}

#[tauri::command]
pub async fn ensure_local_gateway(
    app: AppHandle,
    connection_id: String,
    base_url: String,
    python_path: Option<String>,
    credential_ref: Option<String>,
    operation_id: String,
) -> Result<EnsureGatewayResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let progress = |stage: &str, detail: &str| {
            let _ = app.emit(
                "gateway-startup-progress",
                GatewayStartupProgress {
                    connection_id: connection_id.clone(),
                    operation_id: operation_id.clone(),
                    stage: stage.to_string(),
                    detail: detail.to_string(),
                },
            );
        };
        let result = ensure_local_gateway_blocking(
            app.state::<GatewayProcesses>(),
            connection_id.clone(),
            base_url,
            python_path,
            credential_ref,
            &progress,
        );
        match &result {
            Ok(_) => progress("ready", "本地 Gateway 已就绪"),
            Err(error) => progress("error", error),
        }
        result
    })
    .await
    .map_err(|error| format!("Gateway startup task failed: {error}"))?
}

fn ensure_local_gateway_blocking(
    processes: tauri::State<'_, GatewayProcesses>,
    connection_id: String,
    base_url: String,
    python_path: Option<String>,
    credential_ref: Option<String>,
    progress: &(impl Fn(&str, &str) + Sync),
) -> Result<EnsureGatewayResult, String> {
    let base = parse_base_url(&base_url)?;
    if !is_loopback(&base) {
        return Err(
            "Desktop may only install and start Gateway processes for loopback addresses"
                .to_string(),
        );
    }
    // Several saved local connections can start together. Serialize provisioning
    // so they cannot run pip or spawn a process for the same environment twice.
    let _startup = match processes.startup.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::WouldBlock) => {
            progress("waiting_setup", "正在等待其他本地环境初始化完成");
            processes
                .startup
                .lock()
                .map_err(|_| "Gateway startup registry is unavailable")?
        }
        Err(std::sync::TryLockError::Poisoned(_)) => {
            return Err("Gateway startup registry is unavailable".to_string());
        }
    };
    progress("checking_gateway", "正在检查本地 Gateway");
    if let Some(health) = probe_health(&base, credential_ref.as_deref())? {
        return Ok(EnsureGatewayResult {
            ready: true,
            started_by_desktop: processes
                .children
                .lock()
                .map(|items| items.contains_key(&connection_id))
                .unwrap_or(false),
            python: None,
            version: health
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string),
            message: "Gateway is ready".to_string(),
        });
    }

    progress("checking_python", "正在检测 Python 环境");
    #[cfg(debug_assertions)]
    let python = {
        let python = detect_development_python(python_path.as_deref())?;
        progress(
            "environment",
            &format!("开发模式直接使用 Python 环境：{python}"),
        );
        python
    };
    #[cfg(not(debug_assertions))]
    let python = {
        let base_python = detect_python(python_path.as_deref())?;
        progress(
            "environment",
            &format!("用于创建独立环境的 Python：{base_python}"),
        );
        progress("creating_environment", "正在准备 CrabCode 独立 Python 环境");
        ensure_managed_gateway_python(&base_python, &|line| progress("creating_environment", line))?
    };
    if let Some(environment) = python_environment(&python) {
        progress(
            "environment",
            &format!(
                "本地启动环境：Python {} · {}",
                environment.version, environment.kind
            ),
        );
        progress(
            "environment",
            &format!("Python 解释器路径：{}", environment.executable),
        );
        progress(
            "environment",
            &format!("Python 环境目录：{}", environment.prefix),
        );
    } else {
        progress(
            "environment",
            "无法读取 Python 环境详情，将继续检查 Gateway 安装",
        );
    }
    progress(
        "checking_package",
        &format!("正在检查 CrabCode 安装版本 · {python}"),
    );
    let installed_version = installed_gateway_version(&python);
    progress(
        "environment",
        &format!(
            "本地 Gateway 已安装版本：{}；桌面端需要版本：{}",
            installed_version.as_deref().unwrap_or("未安装或无法导入"),
            env!("CARGO_PKG_VERSION")
        ),
    );
    if installed_version.as_deref() != Some(env!("CARGO_PKG_VERSION")) {
        progress(
            "installing",
            "正在安装 CrabCode 和依赖，首次启动可能需要几分钟",
        );
        install_gateway(&python, &|line| progress("installing", line))?;
    }
    progress("starting_gateway", "正在启动本地 Gateway");
    let host = base.host_str().unwrap_or("127.0.0.1");
    let port = base.port_or_known_default().unwrap_or(4096).to_string();
    let mut command = Command::new(&python);
    configure_gateway_command(&mut command, host, &port);
    if let Some(home) = dirs::home_dir() {
        command.current_dir(home);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Unable to start the local Gateway: {error}"))?;
    processes
        .children
        .lock()
        .map_err(|_| "Gateway process registry is unavailable".to_string())?
        .insert(connection_id.clone(), child);

    progress("waiting_gateway", "正在等待本地 Gateway 就绪");
    for _ in 0..30 {
        thread::sleep(Duration::from_millis(350));
        if let Some(health) = probe_health(&base, credential_ref.as_deref())? {
            return Ok(EnsureGatewayResult {
                ready: true,
                started_by_desktop: true,
                python: Some(python),
                version: health
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                message: "Desktop started the local Gateway".to_string(),
            });
        }
        let exited = processes
            .children
            .lock()
            .map_err(|_| "Gateway process registry is unavailable".to_string())?
            .get_mut(&connection_id)
            .and_then(|process| process.try_wait().ok().flatten())
            .is_some();
        if exited {
            processes
                .children
                .lock()
                .map_err(|_| "Gateway process registry is unavailable".to_string())?
                .remove(&connection_id);
            return Err("The local Gateway process exited before becoming ready".to_string());
        }
    }
    shutdown_gateway(processes.clone(), connection_id)?;
    Err("The local Gateway did not become ready within 10 seconds".to_string())
}

#[tauri::command]
pub fn shutdown_gateway(
    processes: tauri::State<'_, GatewayProcesses>,
    connection_id: String,
) -> Result<bool, String> {
    let child = processes
        .children
        .lock()
        .map_err(|_| "Gateway process registry is unavailable".to_string())?
        .remove(&connection_id);
    let Some(mut child) = child else {
        return Ok(false);
    };
    stop_child_tree(&mut child).map_err(|error| format!("Unable to stop Gateway: {error}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn streams_installer_output_before_exit_and_keeps_failure_details() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("progress-received");
        let mut command = Command::new("/bin/sh");
        // The child can only continue after its first line reaches the callback.
        command
            .args([
                "-c",
                r#"
            printf 'installing\n'
            attempt=0
            while [ ! -f "$1" ]; do
                attempt=$((attempt + 1))
                [ "$attempt" -lt 100 ] || exit 99
                sleep 0.01
            done
            printf 'download failed\n' >&2
            exit 7
        "#,
                "installer-test",
            ])
            .arg(&marker);
        let output = Mutex::new(Vec::new());
        let (status, tail) = run_streaming_command(&mut command, &|line| {
            output.lock().unwrap().push(line.to_string());
            if line == "installing" {
                std::fs::write(&marker, "received").unwrap();
            }
        })
        .unwrap();
        assert_eq!(status.code(), Some(7));
        assert_eq!(*output.lock().unwrap(), ["installing", "download failed"]);
        assert!(tail.contains("download failed"));
    }

    #[cfg(unix)]
    #[test]
    fn bounds_installer_history_without_dropping_live_output() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "i=0; while [ $i -lt 100 ]; do echo line-$i; echo error-$i >&2; i=$((i + 1)); done",
        ]);
        let output = Mutex::new(Vec::new());
        let (status, tail) = run_streaming_command(&mut command, &|line| {
            output.lock().unwrap().push(line.to_string());
        })
        .unwrap();
        assert!(status.success());
        assert_eq!(output.lock().unwrap().len(), 200);
        assert_eq!(tail.lines().count(), 80);
        assert!(tail.contains("line-99"));
        assert!(tail.contains("error-99"));
        assert!(!tail.contains("line-0\n"));
    }

    #[test]
    fn only_loopback_addresses_are_local() {
        assert!(is_loopback(
            &parse_base_url("http://127.0.0.1:4096").unwrap()
        ));
        assert!(is_loopback(
            &parse_base_url("http://localhost:4096").unwrap()
        ));
        assert!(!is_loopback(
            &parse_base_url("https://192.0.2.1:4096").unwrap()
        ));
    }

    #[test]
    fn normalizes_base_urls() {
        assert_eq!(
            parse_base_url("https://example.com:4096").unwrap().as_str(),
            "https://example.com:4096/"
        );
        assert!(parse_base_url("ws://localhost:4096").is_err());
    }

    #[test]
    fn gateway_launch_allows_the_macos_tauri_origin() {
        let mut command = Command::new("python");
        configure_gateway_command(&mut command, "127.0.0.1", "4096");
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--cors", DESKTOP_ORIGIN]));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_conda_python_without_a_shell_path() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let python = directory.path().join("anaconda3/bin/python3");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "#!/bin/sh\nprintf 'Python 3.12.4\\n'\n").unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut candidates = Vec::new();
        extend_macos_python_candidates(&mut candidates, Some(directory.path()));
        let discovered = python.to_string_lossy().into_owned();
        assert!(candidates.contains(&discovered));
        assert_eq!(python_version(&discovered), Some((3, 12)));
    }

    #[cfg(unix)]
    #[test]
    fn creates_and_reuses_an_isolated_gateway_environment() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let base_python = directory.path().join("base-python");
        std::fs::write(
            &base_python,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'Python 3.12.4\n'
  exit 0
fi
if [ "$1" = "-m" ] && [ "$2" = "venv" ] && [ "$3" = "--clear" ]; then
  mkdir -p "$4/bin"
  cp "$0" "$4/bin/python"
  printf 'created isolated environment\n'
  exit 0
fi
exit 2
"#,
        )
        .unwrap();
        std::fs::set_permissions(&base_python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let environment = directory.path().join("managed/gateway-venv");
        let output = Mutex::new(Vec::new());
        let capture = |line: &str| output.lock().unwrap().push(line.to_string());

        let first = ensure_managed_gateway_python_at(
            &base_python.to_string_lossy(),
            &environment,
            &capture,
        )
        .unwrap();
        let second = ensure_managed_gateway_python_at(
            &base_python.to_string_lossy(),
            &environment,
            &capture,
        )
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(Path::new(&first), managed_gateway_python_path(&environment));
        assert_eq!(*output.lock().unwrap(), ["created isolated environment"]);
    }

    #[test]
    fn maps_document_engine_install_stages_to_monotonic_progress() {
        let messages = [
            "正在下载高精度 PDF 引擎",
            "正在创建独立 Python 环境",
            "正在从 BabelDOC 官方源安装程序与依赖",
            "正在下载并校验 BabelDOC 官方模型与字体",
            "正在校验高精度 PDF 引擎",
            "正在启用高精度 PDF 引擎",
        ];
        let progress = messages.map(|message| install_progress("test", message).percent);
        assert_eq!(progress, [5, 15, 36, 68, 88, 96]);
        assert!(progress.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
