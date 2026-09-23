//! Install the host VM engine with Lume's official, unprivileged installer.
use super::{lume, output, supported};
use serde::Serialize;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const INSTALL_URL: &str = "https://cua.ai/lume/install.sh";
const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
static INSTALLING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LumeStatus {
    supported: bool,
    available: bool,
    version: Option<String>,
    path: Option<String>,
    reason: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallProgress {
    operation_id: String,
    stage: String,
    detail: String,
    percent: u8,
}

fn compatible_version(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        let word = word.trim_start_matches('v');
        let version = word.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<_> = version.split('.').map(str::parse::<u32>).collect();
        matches!(parts.as_slice(), [Ok(major), Ok(minor), Ok(patch)]
            if (*major, *minor, *patch) > (0, 5, 3)
                || ((*major, *minor, *patch) == (0, 5, 3) && !word.contains('-')))
    })
}

fn probe(path: &Path) -> Result<String, String> {
    let mut command = Command::new(path);
    command.arg("--version");
    let bytes = output(command, vec![], Duration::from_secs(15))?;
    let version = String::from_utf8_lossy(&bytes).trim().to_string();
    if !compatible_version(&version) {
        return Err("需要 Lume 0.5.3 或更新版本，请安装或升级 Lume。".into());
    }
    Ok(version.chars().take(120).collect())
}

fn status() -> LumeStatus {
    let mut state = LumeStatus {
        supported: supported().is_ok(),
        available: false,
        version: None,
        path: None,
        reason: None,
    };
    if !state.supported {
        state.reason = Some("Lume 本地虚拟机需要 Apple Silicon Mac。".into());
    } else if let Ok(path) = lume() {
        state.path = Some(path.display().to_string());
        match probe(&path) {
            Ok(version) => {
                state.version = Some(version);
                state.available = true;
            }
            Err(reason) => state.reason = Some(reason),
        }
    } else {
        state.reason = Some("尚未安装 Lume".into());
    }
    state
}

// Use a file instead of pipes: curl writes progress with carriage returns, and
// either stream can fill a pipe. Retain only a bounded tail when reporting errors.
fn run_installer(mut command: Command, timeout: Duration) -> Result<(), String> {
    let mut log = tempfile::tempfile().map_err(|e| e.to_string())?;
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log.try_clone().map_err(|e| e.to_string())?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("无法启动 Lume 安装程序：{e}"))?;
    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(exit)) => {
                break if exit.success() {
                    Ok(())
                } else {
                    Err(format!("Lume 安装程序失败（{exit}）"))
                }
            }
            Err(error) => break Err(format!("无法等待 Lume 安装程序：{error}")),
            Ok(None) if started.elapsed() >= timeout => {
                break Err("Lume 安装超时，请检查网络后重试。".into())
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    if let Err(reason) = result {
        #[cfg(target_os = "macos")]
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
        let length = log.metadata().map(|m| m.len()).unwrap_or(0);
        let _ = log.seek(SeekFrom::Start(length.saturating_sub(4000)));
        let mut tail = Vec::new();
        let _ = log.take(4000).read_to_end(&mut tail);
        return Err(format!(
            "{reason}\n{}",
            String::from_utf8_lossy(&tail).trim()
        ));
    }
    Ok(())
}

fn install(report: impl Fn(&str, &str, u8)) -> Result<LumeStatus, String> {
    supported()?;
    report("checking", "正在检查 Lume 安装状态", 5);
    let existing = status();
    if existing.available {
        report("complete", "Lume 已可用", 100);
        return Ok(existing);
    }
    let home = dirs::home_dir().ok_or("无法读取用户目录")?;
    // The upstream --no-background-service option also removes an existing
    // daemon. Leave user-managed installations alone in that case.
    if home
        .join("Library/LaunchAgents/com.trycua.lume_daemon.plist")
        .exists()
    {
        return Err(
            "检测到已有 Lume 后台服务，请先按官方文档升级或修复该安装，再重新检测。".into(),
        );
    }
    report("downloading", "正在下载 Lume 官方安装程序", 15);
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(INSTALL_URL)
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|e| format!("下载 Lume 安装程序失败：{e}"))?;
    let mut bytes = Vec::new();
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 || !bytes.starts_with(b"#!/bin/bash") {
        return Err("Lume 官方安装程序内容异常，请稍后重试。".into());
    }
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let script = temporary.path().join("install.sh");
    File::create(&script)
        .and_then(|mut file| file.write_all(&bytes))
        .map_err(|e| e.to_string())?;
    report(
        "installing",
        "正在下载并安装 Lume，首次安装可能需要几分钟",
        35,
    );
    let install_dir = home.join(".local/bin");
    let mut command = Command::new("/bin/bash");
    command
        .args(["--noprofile", "--norc"])
        .arg(&script)
        .args([
            "--no-background-service",
            "--channel",
            "stable",
            "--install-dir",
        ])
        .arg(&install_dir)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TERM", "dumb")
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .env_remove("LUME_VERSION");
    run_installer(command, INSTALL_TIMEOUT)?;
    report("verifying", "正在验证 Lume 版本与可用性", 95);
    let path = install_dir.join("lume");
    let version = probe(&path).map_err(|e| format!("安装后验证失败：{e}"))?;
    report("complete", "Lume 安装完成", 100);
    Ok(LumeStatus {
        supported: true,
        available: true,
        version: Some(version),
        path: Some(path.display().to_string()),
        reason: None,
    })
}

#[tauri::command]
pub async fn lume_install_status() -> Result<LumeStatus, String> {
    tauri::async_runtime::spawn_blocking(status)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn install_lume(app: AppHandle, operation_id: String) -> Result<LumeStatus, String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("Lume 正在安装，请等待当前安装完成。".into());
    }
    struct InstallGuard;
    impl Drop for InstallGuard {
        fn drop(&mut self) {
            INSTALLING.store(false, Ordering::SeqCst);
        }
    }
    // Keep the guard in the worker even if its frontend disappears.
    let guard = InstallGuard;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        install(|stage, detail, percent| {
            let _ = app.emit(
                "lume-install-progress",
                InstallProgress {
                    operation_id: operation_id.clone(),
                    stage: stage.into(),
                    detail: detail.into(),
                    percent,
                },
            );
        })
    })
    .await
    .map_err(|e| format!("Lume 安装任务失败：{e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enforces_minimum_working_version() {
        for version in [
            "0.5.3",
            "lume 0.6.0",
            "Lume v1.0.0",
            "0.5.4-nightly.20260924.1",
        ] {
            assert!(compatible_version(version), "{version}");
        }
        for version in [
            "0.5.2",
            "0.5.3-nightly.20260924.1",
            "unknown",
            "error 2026.9",
            "",
        ] {
            assert!(!compatible_version(version), "{version}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn captures_failure_at_end_of_large_installer_output() {
        let mut command = Command::new("/bin/bash");
        command.args(["-c", "for ((i=0;i<10000;i++)); do echo downloading; done; echo 'download failed' >&2; exit 1"]);
        let error = run_installer(command, Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("download failed"));
        assert!(error.len() < 4500);
    }
    #[cfg(unix)]
    #[test]
    fn installer_timeout_is_bounded_and_retry_can_succeed() {
        let mut command = Command::new("/bin/bash");
        command.args(["-c", "sleep 10"]);
        let start = Instant::now();
        assert!(run_installer(command, Duration::from_millis(100))
            .unwrap_err()
            .contains("超时"));
        assert!(start.elapsed() < Duration::from_secs(3));
        let mut retry = Command::new("/bin/bash");
        retry.args(["-c", "echo installed"]);
        assert!(run_installer(retry, Duration::from_secs(3)).is_ok());
    }
}
