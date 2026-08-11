//! Corporate-separation behavior pinned against the real `prism` binary.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

const PRISM: &str = env!("CARGO_BIN_EXE_prism");

const PLATFORM_ENV: [&str; 14] = [
    "PRISM_API_KEY",
    "PRISM_API_URL",
    "PRISM_PLATFORM_URL",
    "PRISM_TOKEN",
    "PRISM_API_TOKEN",
    "PRISM_PROJECT_ID",
    "PRISM_PLATFORM_PROVIDER",
    "MARC27_API_KEY",
    "MARC27_API_URL",
    "MARC27_PLATFORM_URL",
    "MARC27_TOKEN",
    "MARC27_API_TOKEN",
    "MARC27_PROJECT_ID",
    "MARC27_PLATFORM_PROVIDER",
];

fn prism(root: &Path) -> Command {
    let mut command = Command::new(PRISM);
    command
        .arg("--project-root")
        .arg(root)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env_remove("PRISM_OFFLINE");
    for name in PLATFORM_ENV {
        command.env_remove(name);
    }
    command
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn status_reports_an_explicitly_unconfigured_platform() {
    let root = tempfile::tempdir().unwrap();
    let output = prism(root.path()).arg("status").output().unwrap();
    let (stdout, stderr) = text(&output);

    assert!(output.status.success(), "stderr: {stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(status["platform"], serde_json::Value::Null);
    assert_eq!(status["platform_status"], "not configured");
    assert!(!stdout.contains("marc27.com"));
}

#[test]
fn native_values_win_and_shadowed_aliases_do_not_warn() {
    let root = tempfile::tempdir().unwrap();
    let output = prism(root.path())
        .env("PRISM_API_URL", "https://native.example")
        .env("MARC27_API_URL", "https://legacy.example")
        .env("PRISM_API_KEY", "provider-key")
        .env("MARC27_API_KEY", "m27_legacy")
        .arg("status")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);

    assert!(output.status.success(), "stderr: {stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        status["platform"]["api_base"],
        "https://native.example/api/v1"
    );
    assert_eq!(status["credentials_present"], true);
    assert!(!stderr.contains("deprecated"), "stderr: {stderr}");
}

#[test]
fn status_does_not_report_a_supabase_anon_key_as_user_credentials() {
    let root = tempfile::tempdir().unwrap();
    let output = prism(root.path())
        .env("PRISM_API_URL", "https://project.supabase.co")
        .env("PRISM_PLATFORM_PROVIDER", "supabase")
        .env("PRISM_API_KEY", "public-supabase-anon-key")
        .arg("status")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);

    assert!(output.status.success(), "stderr: {stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(status["platform"]["provider"], "supabase");
    assert_eq!(status["credentials_present"], false);
    assert!(!stdout.contains("public-supabase-anon-key"));
    assert!(!stderr.contains("public-supabase-anon-key"));
}

#[test]
fn used_marc27_aliases_warn_exactly_once_and_still_work() {
    let root = tempfile::tempdir().unwrap();
    let output = prism(root.path())
        .env("MARC27_API_URL", "https://legacy.example")
        .env("MARC27_API_KEY", "m27_legacy")
        .arg("status")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);

    assert!(output.status.success(), "stderr: {stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        status["platform"]["api_base"],
        "https://legacy.example/api/v1"
    );
    let key_notice = "warning: MARC27_API_KEY is deprecated; use PRISM_API_KEY instead.";
    let url_notice = "warning: MARC27_API_URL is deprecated; use PRISM_API_URL instead.";
    assert_eq!(stderr.matches(key_notice).count(), 1, "stderr: {stderr}");
    assert_eq!(stderr.matches(url_notice).count(), 1, "stderr: {stderr}");
}

#[test]
fn legacy_key_only_install_selects_marc27_and_warns_once() {
    let root = tempfile::tempdir().unwrap();
    let output = prism(root.path())
        .env("MARC27_API_KEY", "m27_legacy")
        .arg("status")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);

    assert!(output.status.success(), "stderr: {stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        status["platform"]["api_base"],
        "https://api.marc27.com/api/v1"
    );
    assert_eq!(status["platform"]["provider"], "marc27");
    let notice = "warning: MARC27_API_KEY is deprecated; use PRISM_API_KEY instead.";
    assert_eq!(stderr.matches(notice).count(), 1, "stderr: {stderr}");
}

#[test]
fn project_root_platform_config_is_used_by_status() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".prism")).unwrap();
    std::fs::write(
        root.path().join(".prism/prism.toml"),
        "[platform]\nurl = \"https://configured.example\"\n",
    )
    .unwrap();

    let output = prism(root.path()).arg("status").output().unwrap();
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "stderr: {stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        status["platform"]["api_base"],
        "https://configured.example/api/v1"
    );
}

#[test]
fn project_root_config_reaches_real_auth_and_native_key_header() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]).into_owned();
        let body = r#"{"credits":1.0,"dollar_value":1.0,"org_name":"test"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        request
    });

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".prism")).unwrap();
    std::fs::write(
        root.path().join(".prism/prism.toml"),
        format!("[platform]\nurl = \"http://{address}\"\n"),
    )
    .unwrap();
    let output = prism(root.path())
        .env("PRISM_API_KEY", "provider-key-without-prefix")
        .arg("billing")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    let request = server.join().unwrap();
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: provider-key-without-prefix"),
        "request: {request}"
    );
}

#[test]
fn builtin_workflow_uses_project_config_endpoint_without_url_env() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".prism")).unwrap();
    std::fs::write(
        root.path().join(".prism/prism.toml"),
        "[platform]\nurl = \"https://configured.example\"\n",
    )
    .unwrap();

    let output = prism(root.path())
        .args([
            "workflow",
            "run",
            "forge",
            "--set",
            "paper=paper",
            "--set",
            "dataset=dataset",
            "--set",
            "target=local",
        ])
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("https://configured.example/api/v1"));
}

#[test]
fn login_refuses_before_network_when_no_platform_is_configured() {
    let root = tempfile::tempdir().unwrap();
    let output = prism(root.path())
        .args(["login", "--token", "unused-pat"])
        .output()
        .unwrap();
    let (_stdout, stderr) = text(&output);

    assert!(!output.status.success());
    assert!(
        stderr.contains("No platform configured"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("PRISM_API_URL"), "stderr: {stderr}");
}

#[test]
fn doctor_remains_local_and_explains_the_unconfigured_state() {
    let root = tempfile::tempdir().unwrap();
    let output = prism(root.path()).arg("doctor").output().unwrap();
    let (stdout, stderr) = text(&output);

    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("not configured — running local-only"));
    assert!(stdout.contains("PRISM_API_URL"));
}
