use super::*;
use std::net::TcpListener;

fn unused_address() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    parse_base_url(&format!("http://{}", listener.local_addr().unwrap())).unwrap()
}

fn fixture(directory: &Path, name: &str, mode: &str, version: &str) -> String {
    let script = directory.join("gateway_python.py");
    std::fs::write(&script, include_str!("../tests/fixtures/gateway_python.py")).unwrap();
    let root = directory.join(name);
    let status = Command::new("python3")
        .arg(script)
        .arg(&root)
        .args(["setup", mode, version])
        .status()
        .unwrap();
    assert!(status.success());
    root.join("bin/python").to_string_lossy().into_owned()
}

fn assert_stopped(root: &Path) {
    let pid = std::fs::read_to_string(root.join("started.pid")).unwrap();
    assert!(
        !Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "Gateway {pid} was left running"
    );
}

#[test]
fn prefers_working_external_installation_without_creating_or_installing() {
    let directory = tempfile::tempdir().unwrap();
    let processes = GatewayProcesses::default();
    let root = directory.path();
    let external = fixture(
        root,
        "external with spaces",
        "healthy",
        env!("CARGO_PKG_VERSION"),
    );
    fixture(root, "managed", "healthy", env!("CARGO_PKG_VERSION"));
    let log = Mutex::new(Vec::new());
    let started = start_release_gateway_at(
        vec![external.clone()],
        &root.join("managed"),
        &unused_address(),
        None,
        &processes,
        "test",
        &|stage, _| log.lock().unwrap().push(stage.to_string()),
    )
    .unwrap();
    assert_eq!(started.python, external);
    stop_registered_gateway(&processes, "test").unwrap();
    assert!(!log
        .lock()
        .unwrap()
        .iter()
        .any(|stage| stage == "creating_environment" || stage == "installing"));
    assert!(!root.join("managed/started.pid").exists());
    assert!(!root.join("external with spaces/created-venv").exists());
    assert!(!root.join("external with spaces/ran-pip").exists());
}

#[test]
fn skips_wrong_versions_missing_dependencies_and_failed_startups() {
    let directory = tempfile::tempdir().unwrap();
    let processes = GatewayProcesses::default();
    let root = directory.path();
    let mut candidates = vec![fixture(root, "old", "healthy", "0.0.0")];
    for mode in [
        "dependency",
        "cli",
        "protocol",
        "exit",
        "health_version",
        "health_protocol",
        "healthy",
    ] {
        candidates.push(fixture(root, mode, mode, env!("CARGO_PKG_VERSION")));
    }
    let expected = candidates.last().unwrap().clone();
    let log = Mutex::new(String::new());
    let started = start_release_gateway_at(
        candidates,
        &root.join("managed"),
        &unused_address(),
        None,
        &processes,
        "test",
        &|_, detail| {
            log.lock().unwrap().push_str(detail);
        },
    )
    .unwrap();
    stop_registered_gateway(&processes, "test").unwrap();
    assert_eq!(started.python, expected);
    assert!(!root.join("managed").exists());
    for mode in ["old", "dependency", "cli", "protocol"] {
        assert!(!root.join(mode).join("started.pid").exists());
    }
    for mode in ["exit", "health_version", "health_protocol"] {
        assert_stopped(&root.join(mode));
    }
    let log = log.lock().unwrap();
    for reason in [
        "Desktop requires",
        "missing_gateway_dependency",
        "missing CLI entry point",
        "does not support protocol",
        "fixture startup failure",
        "required version",
    ] {
        assert!(log.contains(reason), "Missing diagnostic: {reason}\n{log}");
    }
}

#[test]
fn falls_back_to_managed_installation_without_mutating_external_python() {
    let directory = tempfile::tempdir().unwrap();
    let processes = GatewayProcesses::default();
    let root = directory.path();
    let external = fixture(root, "external", "dependency", env!("CARGO_PKG_VERSION"));
    let environment = root.join("managed");
    let address = unused_address();
    let started = start_release_gateway_at(
        vec![external.clone()],
        &environment,
        &address,
        None,
        &processes,
        "test",
        &|_, _| {},
    )
    .unwrap();
    stop_registered_gateway(&processes, "test").unwrap();
    assert_eq!(
        Path::new(&started.python),
        managed_gateway_python_path(&environment)
    );
    assert!(root.join("external/created-venv").exists());
    assert!(environment.join("ran-pip").exists());
    assert!(!root.join("external/ran-pip").exists());
    assert!(check_gateway_installation(&external).is_err());

    // A second launch reuses that environment without recreating it or running pip.
    std::fs::remove_file(root.join("external/created-venv")).unwrap();
    std::fs::remove_file(environment.join("ran-pip")).unwrap();
    start_release_gateway_at(
        vec![external],
        &environment,
        &address,
        None,
        &processes,
        "test",
        &|_, _| {},
    )
    .unwrap();
    stop_registered_gateway(&processes, "test").unwrap();
    assert!(!root.join("external/created-venv").exists());
    assert!(!environment.join("ran-pip").exists());
}

#[test]
fn startup_timeout_reaps_the_candidate_process() {
    let directory = tempfile::tempdir().unwrap();
    let processes = GatewayProcesses::default();
    let python = fixture(directory.path(), "hang", "hang", env!("CARGO_PKG_VERSION"));
    let result = start_gateway(
        &python,
        &unused_address(),
        None,
        Duration::from_secs(2),
        &processes,
        "test",
        &|_, _| {},
    );
    assert!(result.err().unwrap().contains("did not become ready"));
    assert_stopped(&directory.path().join("hang"));
}

#[test]
fn import_probe_timeout_is_bounded() {
    let mut command = Command::new("python3");
    command.args(["-c", "import time; time.sleep(60)"]);
    let start = Instant::now();
    let error = run_probe_command(&mut command, Duration::from_millis(150)).unwrap_err();
    assert!(error.contains("timed out"));
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn closing_desktop_stops_candidates_and_prevents_fallback_installation() {
    let directory = tempfile::tempdir().unwrap();
    let processes = GatewayProcesses::default();
    let root = directory.path();
    let python = fixture(root, "hang", "hang", env!("CARGO_PKG_VERSION"));
    let environment = root.join("managed");
    let result = start_release_gateway_at(
        vec![python],
        &environment,
        &unused_address(),
        None,
        &processes,
        "test",
        &|stage, _| {
            if stage == "waiting_gateway" {
                let child_id = processes.children.lock().unwrap()["test"].id();
                processes.stop_all();
                assert!(!Command::new("kill")
                    .args(["-0", &child_id.to_string()])
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success());
            }
        },
    );
    assert!(result.err().unwrap().contains("shutting down"));
    assert!(processes.children.lock().unwrap().is_empty());
    assert!(!environment.exists());
}

#[test]
#[ignore = "starts the installed Gateway; set CRABCODE_TEST_PYTHON and run explicitly"]
fn reuses_real_installed_gateway() {
    let python = std::env::var("CRABCODE_TEST_PYTHON").expect("Set CRABCODE_TEST_PYTHON");
    let directory = tempfile::tempdir().unwrap();
    let processes = GatewayProcesses::default();
    let environment = directory.path().join("must-not-be-created");
    let result = start_release_gateway_at(
        vec![python.clone()],
        &environment,
        &unused_address(),
        None,
        &processes,
        "test",
        &|stage, detail| println!("{stage}: {detail}"),
    );
    let started = result.unwrap();
    stop_registered_gateway(&processes, "test").unwrap();
    assert_eq!(started.python, python);
    assert!(!environment.exists());
    assert_eq!(started.health["version"], env!("CARGO_PKG_VERSION"));
}
