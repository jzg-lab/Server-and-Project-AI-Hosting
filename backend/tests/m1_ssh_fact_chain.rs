use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header::CONTENT_TYPE},
};
use http_body_util::BodyExt;
use network_atlas::{
    api,
    contracts::{DiscoveryProviderStatus, DiscoveryRunState},
    discovery::{
        COMPOSE_LS_COMMAND, CONTAINERS_COMMAND, DOCKER_VERSION_COMMAND, DiscoveryRunner,
        DiscoverySuccess, IMAGES_COMMAND, LINUX_IDENTITY_COMMAND, NETWORKS_COMMAND,
        SYSTEMD_UNITS_COMMAND, VOLUMES_COMMAND,
    },
    monitoring::{ProfileStep, host_resource_v1_batch_command, host_resource_v1_profile},
    secrets::FileSecretStore,
    ssh::{SshCredential, SshFailure, SshLimits, SshTarget, SystemSsh},
    storage,
};
use russh::{
    Channel, ChannelId, MethodKind, MethodSet,
    keys::{Algorithm, EcdsaCurve, PrivateKey, ssh_key},
    server::{self, Msg, Server as _, Session},
};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tower::ServiceExt;

const RAW_DOCUMENT_TOKEN: &str = "fixture_document_token_123456789";

#[derive(Clone)]
struct FixtureServer {
    accept_auth: bool,
    password: Option<String>,
    password_mode: PasswordFixtureMode,
    command_mode: CommandFixtureMode,
}

#[derive(Clone, Copy)]
enum PasswordFixtureMode {
    Password,
    KeyboardInteractive,
}

#[derive(Clone, Copy)]
enum CommandFixtureMode {
    Normal,
    DockerPermissionDenied,
    DockerUnavailable,
    SystemdUnavailable,
}

impl server::Server for FixtureServer {
    type Handler = Self;

    fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> Self {
        self.clone()
    }
}

impl server::Handler for FixtureServer {
    type Error = russh::Error;

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: &ssh_key::PublicKey,
    ) -> Result<server::Auth, Self::Error> {
        if self.accept_auth {
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            })
        }
    }

    async fn auth_password(
        &mut self,
        _user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        match self.password_mode {
            PasswordFixtureMode::Password if self.password.as_deref() == Some(password) => {
                Ok(server::Auth::Accept)
            }
            PasswordFixtureMode::KeyboardInteractive => {
                let methods = MethodSet::from(&[MethodKind::KeyboardInteractive][..]);
                Ok(server::Auth::Reject {
                    proceed_with_methods: Some(methods),
                    partial_success: false,
                })
            }
            PasswordFixtureMode::Password => Ok(server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }),
        }
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<server::Auth, Self::Error> {
        match (self.password_mode, response) {
            (PasswordFixtureMode::KeyboardInteractive, Some(mut response)) => {
                let supplied = response
                    .next()
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_string());
                if supplied.as_deref() == self.password.as_deref() && response.next().is_none() {
                    Ok(server::Auth::Accept)
                } else {
                    Ok(server::Auth::Reject {
                        proceed_with_methods: None,
                        partial_success: false,
                    })
                }
            }
            (PasswordFixtureMode::KeyboardInteractive, None) => Ok(server::Auth::Partial {
                name: Cow::Borrowed("password"),
                instructions: Cow::Borrowed(""),
                prompts: Cow::Owned(vec![(Cow::Borrowed("Password: "), false)]),
            }),
            (PasswordFixtureMode::Password, _) => Ok(server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }),
        }
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data);
        let (stdout, stderr, exit_status) = fixture_command(&command, self.command_mode);
        session.channel_success(channel)?;
        if !stdout.is_empty() {
            session.data(channel, stdout.into_bytes())?;
        }
        if !stderr.is_empty() {
            session.extended_data(channel, 1, stderr.into_bytes())?;
        }
        session.exit_status_request(channel, exit_status)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

fn fixture_command(command: &str, mode: CommandFixtureMode) -> (String, String, u32) {
    if command == host_resource_v1_batch_command() {
        return (monitor_batch_fixture_output(), String::new(), 0);
    }
    if command == LINUX_IDENTITY_COMMAND {
        return (
            "Linux\n6.8.0-fixture\nubuntu\n22.04\n".to_owned(),
            String::new(),
            0,
        );
    }
    if matches!(mode, CommandFixtureMode::DockerPermissionDenied)
        && command == DOCKER_VERSION_COMMAND
    {
        return (
            String::new(),
            "Got permission denied while trying to connect to the Docker daemon socket".to_owned(),
            1,
        );
    }
    if matches!(mode, CommandFixtureMode::DockerUnavailable) && command == DOCKER_VERSION_COMMAND {
        return (
            String::new(),
            "bash: line 1: docker: command not found".to_owned(),
            127,
        );
    }
    if matches!(mode, CommandFixtureMode::SystemdUnavailable) && command == SYSTEMD_UNITS_COMMAND {
        return (
            String::new(),
            "System has not been booted with systemd as init system".to_owned(),
            1,
        );
    }
    if command == SYSTEMD_UNITS_COMMAND {
        return (
            concat!(
                "fixture-api.service loaded active running Fixture API\n",
                "fixture-worker.service loaded inactive dead Fixture Worker\n",
            )
            .to_owned(),
            String::new(),
            0,
        );
    }
    if command == DOCKER_VERSION_COMMAND {
        return (
            r#"{"version":"26.1.0","api_version":"1.45","os":"linux","arch":"amd64"}
"#
            .to_owned(),
            String::new(),
            0,
        );
    }
    if command == COMPOSE_LS_COMMAND {
        return (
            r#"[{"Name":"fixture-stack","Status":"running(1)","ConfigFiles":"/srv/fixture/compose.yml"}]
"#
            .to_owned(),
            String::new(),
            0,
        );
    }
    if command == CONTAINERS_COMMAND {
        return (
            r#"{"id":"container-123","name":"fixture-api","image":"fixture/api:1","state":"running","status":"Up 5 minutes (healthy)","ports":"0.0.0.0:8080->8080/tcp","networks":"fixture-net","mounts":"fixture-data","created_at":"2026-08-11 00:00:00 +0000 UTC","compose_project":"fixture-stack","compose_service":"api","compose_working_dir":"/srv/fixture","env":"TOKEN=must-not-pass"}
"#
            .to_owned(),
            String::new(),
            0,
        );
    }
    if command == IMAGES_COMMAND {
        return (
            r#"{"id":"sha256:image-123","repository":"fixture/api","tag":"1","digest":"sha256:digest","created_at":"2026-08-10","size":"42MB","env":"SECRET=drop"}
"#
            .to_owned(),
            String::new(),
            0,
        );
    }
    if command == NETWORKS_COMMAND {
        return (
            r#"{"id":"network-123","name":"fixture-net","driver":"bridge","scope":"local"}
"#
            .to_owned(),
            String::new(),
            0,
        );
    }
    if command == VOLUMES_COMMAND {
        return (
            r#"{"name":"fixture-data","driver":"local","scope":"local"}
"#
            .to_owned(),
            String::new(),
            0,
        );
    }
    if command.starts_with("LC_ALL=C find -- ") {
        return (
            "/srv/fixture/README.md\n/srv/fixture/.env\n/srv/fixture/docs/design.md\n/srv/fixture/compose.yml\n".to_owned(),
            String::new(),
            0,
        );
    }
    if command.contains("/srv/fixture/README.md") {
        let body = format!(
            "# Fixture project\nAPI_KEY=visible-before-redaction\ntoken: {RAW_DOCUMENT_TOKEN}\n"
        );
        return (
            format!("{}\n{}\n{}", body.len(), "a".repeat(64), body),
            String::new(),
            0,
        );
    }
    if command.contains("/srv/fixture/docs/design.md") {
        let body = "# Design\nThe fixture exposes one compose service.\n";
        return (
            format!("{}\n{}\n{}", body.len(), "b".repeat(64), body),
            String::new(),
            0,
        );
    }
    if command.contains("/srv/fixture/compose.yml") {
        let body = r#"name: fixture-stack
services:
  api:
    image: fixture/api:1
    ports: ["8080:8080"]
    networks: [fixture-net]
    volumes: ["fixture-data:/data"]
    labels:
      app.role: api
      private.token: must-not-pass
    environment:
      API_TOKEN: must-not-pass
networks:
  fixture-net: {}
volumes:
  fixture-data: {}
secrets:
  production_key:
    file: ./secret.pem
"#;
        return (
            format!("{}\n{}\n{}", body.len(), "c".repeat(64), body),
            String::new(),
            0,
        );
    }
    (String::new(), "unsupported fixture command".to_owned(), 127)
}

fn monitor_batch_fixture_output() -> String {
    let mut output = String::new();
    for step in host_resource_v1_profile() {
        let ProfileStep::Action(action) = step else {
            continue;
        };
        output.push_str("__NETWORK_ATLAS_HOST_RESOURCE_V1_BEGIN__");
        output.push_str(action.action_id);
        output.push('\n');
        let (body, exit_code) = match action.action_id {
            "frame_a_boot_uptime" => ("fixture-boot\n100.0 10.0", 0),
            "frame_b_boot_uptime" => ("fixture-boot\n101.0 10.5", 0),
            "frame_a_cpu" => ("cpu 100 10 50 800 20 10 5 5 0 0", 0),
            "frame_b_cpu" => ("cpu 150 10 70 850 30 10 10 10 0 0", 0),
            "frame_a_disk_io" => ("8 0 sda 100 0 200 50 300 0 400 60 0 70 80", 0),
            "frame_b_disk_io" => ("8 0 sda 110 0 220 55 305 0 430 70 0 90 110", 0),
            "frame_a_network" => (
                "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n eth0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0",
                0,
            ),
            "frame_b_network" => (
                "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n eth0: 3000 30 0 0 0 0 0 0 5000 50 0 0 0 0 0 0",
                0,
            ),
            "frame_a_network_identity" | "frame_b_network_identity" => ("eth0\t2\t2\tup\t1000", 0),
            "cpu_online" => ("0-1", 0),
            "memory" => (
                "MemTotal: 1000000 kB\nMemAvailable: 400000 kB\nSwapTotal: 200000 kB\nSwapFree: 150000 kB\nCached: 120000 kB\nBuffers: 10000 kB\nSlab: 30000 kB",
                0,
            ),
            "load" => ("0.50 1.00 1.50 2/100 1234", 0),
            "disk_capacity" => (
                "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda1 1000 400 500 45% /",
                0,
            ),
            "disk_inodes" => (
                "Filesystem Inodes IUsed IFree IUse% Mounted on\n/dev/sda1 1000 400 500 45% /",
                0,
            ),
            "process_summary" => (
                "scanned=120 running=3 blocked=2 zombie=1 raced=4 truncated=0",
                0,
            ),
            _ => ("", 127),
        };
        if !body.is_empty() {
            output.push_str(body);
            output.push('\n');
        }
        output.push_str("__NETWORK_ATLAS_HOST_RESOURCE_V1_END__");
        output.push_str(action.action_id);
        output.push(':');
        output.push_str(&exit_code.to_string());
        output.push('\n');
    }
    output
}

struct SshFixture {
    port: u16,
    handle: server::RunningServerHandle,
    task: JoinHandle<std::io::Result<()>>,
}

impl SshFixture {
    async fn start(port: Option<u16>, accept_auth: bool) -> Self {
        Self::start_with_command_mode(port, accept_auth, CommandFixtureMode::Normal).await
    }

    async fn start_with_command_mode(
        port: Option<u16>,
        accept_auth: bool,
        command_mode: CommandFixtureMode,
    ) -> Self {
        let host_key = PrivateKey::random(
            &mut rand::rng(),
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
        )
        .expect("fixture host key");
        let config = Arc::new(server::Config {
            inactivity_timeout: Some(Duration::from_secs(30)),
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            keys: vec![host_key],
            ..Default::default()
        });
        let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
            .await
            .expect("fixture listener");
        let port = listener.local_addr().expect("fixture address").port();
        let (handle_tx, handle_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut server = FixtureServer {
                accept_auth,
                password: None,
                password_mode: PasswordFixtureMode::Password,
                command_mode,
            };
            let running = server.run_on_socket(config, &listener);
            let handle = running.handle();
            assert!(handle_tx.send(handle).is_ok());
            running.await
        });
        let handle = handle_rx.await.expect("fixture server handle");
        Self { port, handle, task }
    }

    async fn start_with_password(password: &str) -> Self {
        Self::start_with_password_mode(
            password,
            PasswordFixtureMode::Password,
            CommandFixtureMode::Normal,
        )
        .await
    }

    async fn start_with_keyboard_interactive_password(password: &str) -> Self {
        Self::start_with_password_mode(
            password,
            PasswordFixtureMode::KeyboardInteractive,
            CommandFixtureMode::Normal,
        )
        .await
    }

    async fn start_with_password_and_docker_permission_denied(password: &str) -> Self {
        Self::start_with_password_mode(
            password,
            PasswordFixtureMode::Password,
            CommandFixtureMode::DockerPermissionDenied,
        )
        .await
    }

    async fn start_with_password_and_docker_unavailable(password: &str) -> Self {
        Self::start_with_password_mode(
            password,
            PasswordFixtureMode::Password,
            CommandFixtureMode::DockerUnavailable,
        )
        .await
    }

    async fn start_with_password_mode(
        password: &str,
        password_mode: PasswordFixtureMode,
        command_mode: CommandFixtureMode,
    ) -> Self {
        let host_key = PrivateKey::random(
            &mut rand::rng(),
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
        )
        .expect("fixture host key");
        let config = Arc::new(server::Config {
            inactivity_timeout: Some(Duration::from_secs(30)),
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            keys: vec![host_key],
            ..Default::default()
        });
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("fixture listener");
        let port = listener.local_addr().expect("fixture address").port();
        let (handle_tx, handle_rx) = oneshot::channel();
        let password = password.to_owned();
        let task = tokio::spawn(async move {
            let mut server = FixtureServer {
                accept_auth: false,
                password: Some(password),
                password_mode,
                command_mode,
            };
            let running = server.run_on_socket(config, &listener);
            let handle = running.handle();
            assert!(handle_tx.send(handle).is_ok());
            running.await
        });
        let handle = handle_rx.await.expect("fixture server handle");
        Self { port, handle, task }
    }

    async fn stop(mut self) {
        self.handle.shutdown("test fixture shutdown".to_owned());
        timeout(Duration::from_secs(5), &mut self.task)
            .await
            .expect("fixture shutdown deadline")
            .expect("fixture task join")
            .expect("fixture server shutdown");
    }
}

fn ssh_binary() -> &'static str {
    if cfg!(windows) { "ssh.exe" } else { "ssh" }
}

fn keyscan_binary() -> &'static str {
    if cfg!(windows) {
        "ssh-keyscan.exe"
    } else {
        "ssh-keyscan"
    }
}

fn keygen_binary() -> &'static str {
    if cfg!(windows) {
        "ssh-keygen.exe"
    } else {
        "ssh-keygen"
    }
}

fn generate_client_key(directory: &Path) -> (PathBuf, String) {
    let path = directory.join("fixture-client-key");
    let output = Command::new(keygen_binary())
        .arg("-q")
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg("")
        .arg("-f")
        .arg(&path)
        .output()
        .expect("ssh-keygen starts");
    assert!(
        output.status.success(),
        "ssh-keygen failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let private_key = std::fs::read_to_string(&path).expect("fixture private key");
    (path, private_key)
}

async fn test_app(directory: &TempDir) -> (Router, SqlitePool, PathBuf) {
    let database_path = directory.path().join("m1.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let pool = storage::connect(&database_url)
        .await
        .expect("test database");
    let secret_root = directory.path().join("secrets");
    let ssh_state = directory.path().join("ssh-state");
    let ssh = SystemSsh::new(
        ssh_binary(),
        keyscan_binary(),
        ssh_state,
        SshLimits {
            connect_timeout: Duration::from_secs(3),
            command_timeout: Duration::from_secs(5),
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
        },
    );
    let state = api::AppState::with_services(pool.clone(), FileSecretStore::new(&secret_root), ssh);
    (api::router(state, "../frontend"), pool, secret_root)
}

async fn json_request(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    idempotency_key: Option<&str>,
) -> (StatusCode, Value, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    let request_body = if let Some(body) = body {
        builder = builder.header(CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&body).expect("request JSON"))
    } else {
        Body::empty()
    };
    if let Some(key) = idempotency_key {
        builder = builder.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(builder.body(request_body).expect("request"))
        .await
        .expect("router response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    let text = String::from_utf8(bytes.to_vec()).expect("UTF-8 response");
    let json = serde_json::from_str(&text).expect("JSON response");
    (status, json, text)
}

async fn register_host(
    app: &Router,
    private_key: &str,
    port: u16,
) -> (String, String, String, Vec<String>, Value) {
    let secret_request = json!({"kind": "ssh_key", "private_key": private_key});
    let (status, secret, secret_text) = json_request(
        app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(secret_request.clone()),
        Some("secret-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{secret_text}");
    assert!(!secret_text.contains("BEGIN OPENSSH PRIVATE KEY"));
    let credential_ref = secret["data"]["credential_ref"]
        .as_str()
        .expect("credential reference")
        .to_owned();

    let (status, secret_replay, replay_text) = json_request(
        app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(secret_request),
        Some("secret-create"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay_text}");
    assert_eq!(
        secret_replay["data"]["credential_ref"],
        secret["data"]["credential_ref"]
    );
    let (status, reused, reused_text) = json_request(
        app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(json!({"kind": "ssh_key", "private_key": format!("{private_key}\n")})),
        Some("secret-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reused_text}");
    assert_eq!(reused["error"]["code"], "IDEMPOTENCY_KEY_REUSED");

    let host_request = json!({
        "display_name": "Fixture Linux HOST",
        "address": "127.0.0.1",
        "port": port,
        "ssh_user": "fixture",
        "credential_ref": credential_ref,
    });
    let (status, host, host_text) = json_request(
        app,
        Method::POST,
        "/api/v1/hosts",
        Some(host_request.clone()),
        Some("host-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{host_text}");
    let host_id = host["data"]["host_id"]
        .as_str()
        .expect("host id")
        .to_owned();

    let (status, host_replay, replay_text) = json_request(
        app,
        Method::POST,
        "/api/v1/hosts",
        Some(host_request.clone()),
        Some("host-create"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay_text}");
    assert_eq!(host_replay["data"]["host_id"], host["data"]["host_id"]);
    let mut changed_host_request = host_request;
    changed_host_request["display_name"] = json!("Different fixture HOST");
    let (status, reused, reused_text) = json_request(
        app,
        Method::POST,
        "/api/v1/hosts",
        Some(changed_host_request),
        Some("host-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reused_text}");
    assert_eq!(reused["error"]["code"], "IDEMPOTENCY_KEY_REUSED");

    let (status, first, first_text) = json_request(
        app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("fingerprint-preview"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first_text}");
    assert_eq!(
        first["data"]["state"], "host_key_unverified",
        "{first_text}"
    );
    let fingerprint = first["data"]["candidate_fingerprint"]
        .as_str()
        .expect("candidate fingerprint")
        .to_owned();

    let (status, replay, replay_text) = json_request(
        app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("fingerprint-preview"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay_text}");
    assert_eq!(replay["data"]["test_id"], first["data"]["test_id"]);

    let (status, confirmed, confirmation_text) = json_request(
        app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/host-key-confirmations"),
        Some(json!({"fingerprint": fingerprint})),
        Some("confirm-fingerprint"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmation_text}");
    assert_eq!(confirmed["data"]["host_key_state"], "verified");

    let (status, confirmation_replay, replay_text) = json_request(
        app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/host-key-confirmations"),
        Some(json!({"fingerprint": fingerprint})),
        Some("confirm-fingerprint"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay_text}");
    assert_eq!(
        confirmation_replay["data"]["host_id"],
        confirmed["data"]["host_id"]
    );
    let (status, reused, reused_text) = json_request(
        app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/host-key-confirmations"),
        Some(json!({"fingerprint": "SHA256:different"})),
        Some("confirm-fingerprint"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reused_text}");
    assert_eq!(reused["error"]["code"], "IDEMPOTENCY_KEY_REUSED");

    let (status, ready, ready_text) = json_request(
        app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("authenticated-capabilities"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ready_text}");
    let capabilities = ready["data"]["capabilities"]
        .as_array()
        .expect("capabilities")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (credential_ref, host_id, fingerprint, capabilities, ready)
}

async fn wait_for_discovery(app: &Router, run_id: &str) -> Value {
    for _ in 0..200 {
        let (status, run, text) = json_request(
            app,
            Method::GET,
            &format!("/api/v1/discovery-runs/{run_id}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        match run["data"]["state"].as_str() {
            Some("accepted" | "running") => sleep(Duration::from_millis(50)).await,
            _ => return run,
        }
    }
    panic!("discovery did not reach a terminal state");
}

async fn wait_for_monitor(app: &Router, run_id: &str) -> Value {
    for _ in 0..200 {
        let (status, run, text) = json_request(
            app,
            Method::GET,
            &format!("/api/v1/monitor-runs/{run_id}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        match run["data"]["state"].as_str() {
            Some("queued" | "running") => sleep(Duration::from_millis(50)).await,
            _ => return run,
        }
    }
    panic!("monitor run did not reach a terminal state");
}

async fn run_systemd_fixture(directory: &TempDir, fixture: &SshFixture) -> DiscoverySuccess {
    let (_key_path, private_key) = generate_client_key(directory.path());
    let ssh = SystemSsh::new(
        ssh_binary(),
        keyscan_binary(),
        directory.path().join("systemd-ssh-state"),
        SshLimits {
            connect_timeout: Duration::from_secs(3),
            command_timeout: Duration::from_secs(5),
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
        },
    );
    let host_id = "host-systemd";
    let scanned = ssh
        .scan_host_key("127.0.0.1", fixture.port)
        .await
        .expect("fixture host key");
    ssh.confirm_host_key(host_id, &scanned.known_hosts_line)
        .await
        .expect("confirmed fixture host key");
    DiscoveryRunner::new(ssh)
        .run_systemd(
            "run-systemd",
            &SshTarget {
                host_id: host_id.to_owned(),
                address: "127.0.0.1".to_owned(),
                port: fixture.port,
                user: "fixture".to_owned(),
                credential: SshCredential::PrivateKey(private_key.into()),
            },
        )
        .await
        .expect("SSH/Linux baseline remains available")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn systemd_provider_returns_structured_service_evidence() {
    let directory = TempDir::new().expect("test directory");
    let fixture = SshFixture::start(None, true).await;
    let success = run_systemd_fixture(&directory, &fixture).await;

    assert_eq!(success.state, DiscoveryRunState::DiscoveryComplete);
    assert_eq!(success.evidence.systemd_units.len(), 2);
    assert_eq!(
        success.evidence.systemd_units[0].metadata["unit"],
        "fixture-api.service"
    );
    let provider = &success.evidence.provider_results[0];
    assert_eq!(provider.provider_kind, "systemd");
    assert_eq!(provider.status, DiscoveryProviderStatus::Ready);
    assert_eq!(provider.observed_count, 2);
    assert_eq!(provider.evidence_refs.len(), 2);
    assert!(provider.warnings.is_empty());
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn systemd_unavailable_is_provider_coverage_not_an_ssh_failure() {
    let directory = TempDir::new().expect("test directory");
    let fixture =
        SshFixture::start_with_command_mode(None, true, CommandFixtureMode::SystemdUnavailable)
            .await;
    let success = run_systemd_fixture(&directory, &fixture).await;

    assert_eq!(success.state, DiscoveryRunState::DiscoveryUnavailable);
    assert_eq!(success.evidence.host_facts.len(), 1);
    assert!(success.evidence.systemd_units.is_empty());
    let provider = &success.evidence.provider_results[0];
    assert_eq!(provider.status, DiscoveryProviderStatus::Unavailable);
    assert_eq!(provider.observed_count, 0);
    assert_eq!(provider.warnings[0].code, "SYSTEMD_UNAVAILABLE");
    assert!(
        success
            .audits
            .iter()
            .any(|audit| audit.action == "systemd_units")
    );
    fixture.stop().await;
}

async fn database_text(pool: &SqlitePool) -> String {
    let queries = [
        "SELECT credential_ref || kind || idempotency_key || secret_sha256 || response_json FROM secret_ref_descriptors",
        "SELECT display_name || address || ssh_user || credential_ref || COALESCE(host_key_fingerprint, '') || status FROM hosts",
        "SELECT idempotency_key || request_sha256 || host_id || response_json FROM host_registration_requests",
        "SELECT request_id || idempotency_key || state || COALESCE(error_summary, '') || response_json FROM connection_tests",
        "SELECT request_id || idempotency_key || fingerprint || response_json FROM host_key_confirmations",
        "SELECT request_id || idempotency_key || state || COALESCE(failure_summary, '') || COALESCE(evidence_json, '') FROM discovery_runs",
        "SELECT source || metadata_json FROM evidence_items",
        "SELECT action || COALESCE(stderr_summary, '') FROM discovery_command_audits",
        "SELECT request_id || idempotency_key || state || COALESCE(failure_summary, '') || coverage_json FROM monitor_runs",
        "SELECT snapshot_json || coverage_json FROM monitoring_current",
    ];
    let mut text = String::new();
    for query in queries {
        let rows = sqlx::query(query)
            .fetch_all(pool)
            .await
            .expect("leak query");
        for row in rows {
            text.push_str(&row.try_get::<String, _>(0).expect("text column"));
        }
    }
    text
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_openssh_protocol_produces_structured_redacted_evidence_and_stops_on_key_change() {
    let directory = TempDir::new().expect("test directory");
    let fixture = SshFixture::start(None, true).await;
    let fixture_port = fixture.port;
    let (app, pool, secret_root) = test_app(&directory).await;
    let (_original_key_path, private_key) = generate_client_key(directory.path());
    let (credential_ref, host_id, _fingerprint, capabilities, ready) =
        register_host(&app, &private_key, fixture_port).await;
    assert_eq!(ready["data"]["state"], "connection_ready");
    assert_eq!(capabilities, vec!["ssh", "linux"]);

    let (status, empty_monitoring, empty_monitoring_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/hosts/{host_id}/monitoring"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{empty_monitoring_text}");
    assert_eq!(empty_monitoring["data"]["latest_run"], Value::Null);
    assert_eq!(empty_monitoring["data"]["current_snapshot"], Value::Null);
    assert_eq!(empty_monitoring["data"]["monitor_freshness"], "unknown");

    let run_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitor_runs")
        .fetch_one(&pool)
        .await
        .expect("monitor run count");
    let (status, rejected, rejected_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/monitor-runs"),
        Some(json!({"profile": "host_resource_v1", "command": "cat /etc/shadow"})),
        Some("monitor-command-rejected"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected_text}");
    assert_eq!(rejected["error"]["code"], "INVALID_JSON");
    let run_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitor_runs")
        .fetch_one(&pool)
        .await
        .expect("monitor run count after rejection");
    assert_eq!(run_count_after, run_count_before);

    let monitor_request = json!({"profile": "host_resource_v1"});
    let monitor_uri = format!("/api/v1/hosts/{host_id}/monitor-runs");
    let (status, accepted_monitor, accepted_monitor_text) = json_request(
        &app,
        Method::POST,
        &monitor_uri,
        Some(monitor_request.clone()),
        Some("first-host-monitor"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted_monitor_text}");
    assert_eq!(accepted_monitor["data"]["state"], "queued");
    let monitor_run_id = accepted_monitor["data"]["run_id"]
        .as_str()
        .expect("monitor run id");
    let (status, replay_monitor, replay_monitor_text) = json_request(
        &app,
        Method::POST,
        &monitor_uri,
        Some(monitor_request),
        Some("first-host-monitor"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{replay_monitor_text}");
    assert_eq!(replay_monitor["data"]["run_id"], monitor_run_id);

    let monitor = wait_for_monitor(&app, monitor_run_id).await;
    assert_eq!(monitor["data"]["state"], "succeeded", "{monitor}");
    assert_eq!(monitor["data"]["ssh_session_count"], 1);
    assert_eq!(monitor["data"]["failure_code"], Value::Null);
    let (status, current, current_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/hosts/{host_id}/monitoring"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{current_text}");
    assert_eq!(current["data"]["latest_run"]["run_id"], monitor_run_id);
    assert_eq!(
        current["data"]["current_snapshot"]["run_id"],
        monitor_run_id
    );
    assert_eq!(current["data"]["current_snapshot"]["freshness"], "fresh");
    assert_eq!(current["data"]["current_snapshot"]["ssh_session_count"], 1);
    assert!(
        current["data"]["current_snapshot"]["cpu"]["busy_percent"]
            .as_f64()
            .is_some_and(|value| value > 0.0)
    );
    assert_eq!(
        current["data"]["current_snapshot"]["memory"]["used_bytes"],
        614_400_000u64
    );
    assert_eq!(
        current["data"]["current_snapshot"]["network"][0]["name"],
        "eth0"
    );

    let (status, hosts_view, hosts_view_text) =
        json_request(&app, Method::GET, "/api/v1/views/global/hosts", None, None).await;
    assert_eq!(status, StatusCode::OK, "{hosts_view_text}");
    assert_eq!(
        hosts_view["data"]["hosts"][0]["current_snapshot_run_id"],
        monitor_run_id
    );
    assert!(hosts_view["data"]["hosts"][0]["monitor_observed_at"].is_string());
    assert!(
        hosts_view["data"]["hosts"][0]
            .get("current_snapshot")
            .is_none()
    );
    assert_eq!(hosts_view["data"]["hosts"][0]["monitor_freshness"], "fresh");

    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, failure_code, failure_summary,
            submitted_at, finished_at, accepted_response_json
         ) VALUES (?, ?, ?, ?, ?, 'host_resource_v1', 'manual', 'failed',
                   'COLLECTOR_PROCESS_FAILED', 'Fixed host resource command failed',
                   '9999-12-31T23:59:58Z', '9999-12-31T23:59:59Z', '{}')",
    )
    .bind("monitor-fixture-latest-failure")
    .bind(&host_id)
    .bind("monitor-fixture-latest-failure-request")
    .bind("monitor-fixture-latest-failure-key")
    .bind("monitor-fixture-latest-failure-hash")
    .execute(&pool)
    .await
    .expect("insert failed monitor receipt");
    let (status, fallback, fallback_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/hosts/{host_id}/monitoring"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fallback_text}");
    assert_eq!(fallback["data"]["latest_run"]["state"], "failed");
    assert_eq!(
        fallback["data"]["current_snapshot"]["run_id"],
        monitor_run_id
    );
    assert_eq!(fallback["data"]["monitor_freshness"], "fresh");

    let (status, exported, export_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/hosts/{host_id}/export"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{export_text}");
    assert!(exported["data"]["payload"]["monitor_runs"].is_array());
    assert_eq!(
        exported["data"]["payload"]["monitoring_current"]["snapshot"]["run_id"],
        monitor_run_id
    );
    assert!(
        exported["data"]["payload"]["monitoring_current"]["snapshot"]["filesystems"][0]
            .get("source")
            .is_none()
    );

    let (status, accepted, accepted_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({})),
        Some("first-real-discovery"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted_text}");
    let run_id = accepted["data"]["run_id"]
        .as_str()
        .expect("run id")
        .to_owned();
    let completed = wait_for_discovery(&app, &run_id).await;
    assert_eq!(completed["data"]["state"], "evidence_ready");
    let draft_id = completed["data"]["draft_id"]
        .as_str()
        .expect("deterministic M2 draft after evidence persistence")
        .to_owned();
    assert!(completed["data"]["evidence_item_count"].as_u64().unwrap() >= 8);
    assert_eq!(
        completed["data"]["evidence_sha256"]
            .as_str()
            .expect("evidence hash")
            .len(),
        64
    );

    let (status, draft, draft_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/projection-drafts/{draft_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{draft_text}");
    assert_eq!(draft["meta"]["data_source"]["kind"], "real");
    assert!(draft["data"]["nodes"].as_array().unwrap().len() >= 8);

    let (status, evidence, evidence_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/discovery-runs/{run_id}/evidence"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{evidence_text}");
    assert_eq!(evidence["data"]["protocol_version"], "1");
    assert_eq!(evidence["data"]["host"]["os"], "linux");
    assert_eq!(
        evidence["data"]["compose_projects"][0]["metadata"]["name"],
        "fixture-stack"
    );
    assert_eq!(
        evidence["data"]["containers"][0]["metadata"]["id"],
        "container-123"
    );
    assert!(
        evidence["data"]["containers"][0]["metadata"]
            .get("env")
            .is_none()
    );
    assert_eq!(
        evidence["data"]["document_candidates"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let compose_document = evidence["data"]["document_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["metadata"]["relative_path"] == "compose.yml")
        .expect("structured Compose evidence");
    assert_eq!(
        compose_document["metadata"]["compose"]["services"][0]["name"],
        "api"
    );
    assert!(
        compose_document["metadata"]
            .get("summary_excerpt")
            .is_none()
    );
    assert!(evidence_text.contains("[REDACTED]"));
    assert!(!evidence_text.contains(RAW_DOCUMENT_TOKEN));
    assert!(!evidence_text.contains("visible-before-redaction"));

    let (status, replay, replay_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({
            "provider_kinds": [],
            "root_refs": [],
            "requested_capabilities": []
        })),
        Some("first-real-discovery"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{replay_text}");
    assert_eq!(replay["data"]["run_id"], run_id);
    for changed_body in [
        json!({"provider_kinds": ["systemd"], "requested_capabilities": ["read_only"]}),
        json!({"root_refs": ["/srv/app"]}),
        json!({"requested_capabilities": ["restart"]}),
    ] {
        let (status, conflict, conflict_text) = json_request(
            &app,
            Method::POST,
            &format!("/api/v1/hosts/{host_id}/discovery-runs"),
            Some(changed_body),
            Some("first-real-discovery"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict_text}");
        assert_eq!(conflict["error"]["code"], "IDEMPOTENCY_KEY_REUSED");
    }
    sqlx::query("UPDATE discovery_runs SET request_sha256 = NULL WHERE run_id = ?")
        .bind(&run_id)
        .execute(&pool)
        .await
        .expect("simulate a pre-migration discovery row");
    let (status, legacy_replay, legacy_replay_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({
            "provider_kinds": ["systemd"],
            "requested_capabilities": ["read_only"]
        })),
        Some("first-real-discovery"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{legacy_replay_text}");
    assert_eq!(legacy_replay["data"]["run_id"], run_id);

    let explicit_body = json!({
        "provider_kinds": ["docker", "compose"],
        "requested_capabilities": ["read_only"]
    });
    let (status, explicit, explicit_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(explicit_body),
        Some("explicit-docker-compose"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{explicit_text}");
    let explicit_run_id = explicit["data"]["run_id"]
        .as_str()
        .expect("explicit run id")
        .to_owned();
    let explicit_completed = wait_for_discovery(&app, &explicit_run_id).await;
    assert_eq!(explicit_completed["data"]["state"], "discovery_complete");
    let (status, explicit_evidence, explicit_evidence_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/discovery-runs/{explicit_run_id}/evidence"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{explicit_evidence_text}");
    for provider_kind in ["docker", "compose"] {
        assert!(
            explicit_evidence["data"]["provider_results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|provider| {
                    provider["provider_kind"] == provider_kind && provider["status"] == "ready"
                })
        );
    }
    let (status, normalized_replay, normalized_replay_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({
            "provider_kinds": ["compose", "docker", "docker"],
            "requested_capabilities": ["read_only", "read_only"]
        })),
        Some("explicit-docker-compose"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{normalized_replay_text}");
    assert_eq!(normalized_replay["data"]["run_id"], explicit_run_id);
    let stored_request_hash: Option<String> =
        sqlx::query_scalar("SELECT request_sha256 FROM discovery_runs WHERE run_id = ?")
            .bind(&explicit_run_id)
            .fetch_one(&pool)
            .await
            .expect("stored discovery request hash");
    assert_eq!(stored_request_hash.as_deref().map(str::len), Some(64));

    let credential_id = credential_ref
        .strip_prefix("secret://ssh/")
        .expect("SSH credential id");
    let credential_path = secret_root.join(format!("{credential_id}.key"));
    assert!(credential_path.is_file());
    let stored_key = std::fs::read_to_string(&credential_path).expect("stored key");
    assert_eq!(stored_key, private_key);
    let persisted = database_text(&pool).await;
    assert!(!persisted.contains("BEGIN OPENSSH PRIVATE KEY"));
    assert!(!persisted.contains(RAW_DOCUMENT_TOKEN));
    assert!(!persisted.contains("visible-before-redaction"));
    assert!(!persisted.contains(credential_path.to_string_lossy().as_ref()));

    fixture.stop().await;
    let changed_fixture = SshFixture::start(Some(fixture_port), true).await;
    let (status, changed, changed_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("host-key-after-replacement"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed_text}");
    assert_eq!(changed["data"]["state"], "host_key_changed");
    assert_eq!(changed["data"]["error_code"], "HOST_KEY_CHANGED");

    let (status, blocked, blocked_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({})),
        Some("blocked-after-key-change"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked_text}");
    assert_eq!(blocked["error"]["code"], "CONNECTION_NOT_READY");
    changed_fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ssh_password_authentication_reaches_connection_ready_without_leaking_password() {
    let directory = TempDir::new().expect("test directory");
    let password = "fixture-password-123";
    let fixture = SshFixture::start_with_password(password).await;
    let (app, pool, secret_root) = test_app(&directory).await;

    let (status, secret, secret_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(json!({"kind": "ssh_password", "password": password})),
        Some("password-secret-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{secret_text}");
    assert_eq!(secret["data"]["kind"], "ssh_password");
    assert!(!secret_text.contains(password));
    let credential_ref = secret["data"]["credential_ref"]
        .as_str()
        .expect("password credential ref");
    let credential_id = credential_ref
        .strip_prefix("secret://ssh-password/")
        .expect("password credential id");
    assert!(
        secret_root
            .join(format!("{credential_id}.password"))
            .is_file()
    );

    let (status, host, host_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/hosts",
        Some(json!({
            "display_name": "PASSWORD_HOST",
            "address": "127.0.0.1",
            "port": fixture.port,
            "ssh_user": "fixture",
            "credential_ref": credential_ref
        })),
        Some("password-host-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{host_text}");
    assert_eq!(host["data"]["credential_kind"], "ssh_password");
    let host_id = host["data"]["host_id"].as_str().expect("host id");

    let (_, first, _) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("password-host-key-fetch"),
    )
    .await;
    assert_eq!(first["data"]["state"], "host_key_unverified");
    let fingerprint = first["data"]["candidate_fingerprint"]
        .as_str()
        .expect("candidate fingerprint");
    let (status, confirmed, confirmed_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/host-key-confirmations"),
        Some(json!({"fingerprint": fingerprint})),
        Some("password-host-key-confirm"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmed_text}");
    assert_eq!(confirmed["data"]["host_key_state"], "verified");
    let (status, ready, ready_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("password-authenticated-capabilities"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ready_text}");
    assert_eq!(ready["data"]["state"], "connection_ready", "{ready_text}");
    assert_eq!(ready["data"]["port"], fixture.port);
    assert_eq!(ready["data"]["ssh_user"], "fixture");
    assert_eq!(ready["data"]["credential_kind"], "ssh_password");
    assert_eq!(ready["data"]["auth_transport"], "russh_client");
    assert!(ready["data"].get("credential_ref").is_none());
    assert!(!ready_text.contains(password));
    assert!(!database_text(&pool).await.contains(password));

    let mut legacy_response = ready.clone();
    let legacy_data = legacy_response["data"]
        .as_object_mut()
        .expect("connection test data");
    legacy_data.remove("port");
    legacy_data.remove("ssh_user");
    legacy_data.remove("credential_kind");
    legacy_data.remove("auth_transport");
    sqlx::query(
        "UPDATE connection_tests SET response_json = ?
         WHERE host_id = ? AND idempotency_key = 'password-authenticated-capabilities'",
    )
    .bind(serde_json::to_string(&legacy_response).expect("legacy response JSON"))
    .bind(host_id)
    .execute(&pool)
    .await
    .expect("store legacy connection test response");
    let (status, replayed_connection, replayed_connection_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("password-authenticated-capabilities"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replayed_connection_text}");
    assert_eq!(
        replayed_connection["data"]["test_id"],
        ready["data"]["test_id"]
    );
    assert_eq!(replayed_connection["data"]["port"], fixture.port);
    assert_eq!(replayed_connection["data"]["ssh_user"], "fixture");
    assert_eq!(
        replayed_connection["data"]["credential_kind"],
        "ssh_password"
    );
    assert!(replayed_connection["data"]["auth_transport"].is_null());
    assert!(replayed_connection["data"].get("credential_ref").is_none());
    assert!(!replayed_connection_text.contains(password));

    let password_request_hash: String = sqlx::query_scalar(
        "SELECT secret_sha256 FROM secret_ref_descriptors WHERE credential_ref = ?",
    )
    .bind(credential_ref)
    .fetch_one(&pool)
    .await
    .expect("password request verifier");
    assert!(password_request_hash.starts_with("$argon2id$"));
    assert_ne!(password_request_hash.len(), 64);

    let (status, replayed, replayed_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(json!({"kind": "ssh_password", "password": password})),
        Some("password-secret-create"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replayed_text}");
    assert_eq!(replayed["data"]["credential_ref"], credential_ref);
    assert!(!replayed_text.contains(password));

    let (status, accepted, accepted_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({})),
        Some("password-authenticated-discovery"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted_text}");
    let run_id = accepted["data"]["run_id"]
        .as_str()
        .expect("password discovery run id");
    let completed = wait_for_discovery(&app, run_id).await;
    assert_eq!(completed["data"]["state"], "evidence_ready");
    assert!(completed["data"]["evidence_item_count"].as_u64().unwrap() >= 8);
    let (status, evidence, evidence_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/discovery-runs/{run_id}/evidence"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{evidence_text}");
    assert_eq!(evidence["data"]["host"]["os"], "linux");
    assert!(!evidence_text.contains(password));

    let replacement = "fixture-password-456";
    let (status, replacement_secret, replacement_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(json!({"kind": "ssh_password", "password": replacement})),
        Some("password-secret-replacement"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replacement_text}");
    assert!(!replacement_text.contains(replacement));
    let replacement_ref = replacement_secret["data"]["credential_ref"]
        .as_str()
        .expect("replacement credential ref");
    let (status, updated, updated_text) = json_request(
        &app,
        Method::PATCH,
        &format!("/api/v1/hosts/{host_id}"),
        Some(json!({"credential_ref": replacement_ref})),
        Some("password-host-credential-update"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated_text}");
    assert_eq!(updated["data"]["credential_kind"], "ssh_password");
    assert_eq!(updated["data"]["host_key_state"], "unverified");
    assert_eq!(updated["data"]["status"], "host_registered");
    assert!(!updated_text.contains(replacement));
    assert!(!database_text(&pool).await.contains(replacement));

    let (status, fingerprint_check, fingerprint_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("password-replacement-fingerprint"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fingerprint_text}");
    assert_eq!(fingerprint_check["data"]["state"], "host_key_unverified");
    let replacement_fingerprint = fingerprint_check["data"]["candidate_fingerprint"]
        .as_str()
        .expect("replacement candidate fingerprint");
    let (status, _, confirmation_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/host-key-confirmations"),
        Some(json!({"fingerprint": replacement_fingerprint})),
        Some("password-replacement-fingerprint-confirm"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmation_text}");
    let (status, rejected, rejected_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("password-replacement-rejected"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rejected_text}");
    assert_eq!(rejected["data"]["state"], "failed");
    assert_eq!(rejected["data"]["error_code"], "SSH_AUTH_FAILED");
    assert_eq!(rejected["data"]["auth_transport"], "russh_client");
    assert!(!rejected_text.contains(replacement));
    assert!(!database_text(&pool).await.contains(replacement));
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ssh_password_credential_also_supports_single_password_keyboard_interactive() {
    let directory = TempDir::new().expect("test directory");
    let password = "fixture-keyboard-password-123";
    let fixture = SshFixture::start_with_keyboard_interactive_password(password).await;
    let (app, pool, _secret_root) = test_app(&directory).await;

    let (status, secret, secret_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(json!({"kind": "ssh_password", "password": password})),
        Some("keyboard-password-secret-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{secret_text}");
    assert!(!secret_text.contains(password));
    let credential_ref = secret["data"]["credential_ref"]
        .as_str()
        .expect("password credential ref");

    let (status, host, host_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/hosts",
        Some(json!({
            "display_name": "KEYBOARD_PASSWORD_HOST",
            "address": "127.0.0.1",
            "port": fixture.port,
            "ssh_user": "fixture",
            "credential_ref": credential_ref
        })),
        Some("keyboard-password-host-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{host_text}");
    let host_id = host["data"]["host_id"].as_str().expect("host id");

    let (_, first, first_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("keyboard-password-host-key-fetch"),
    )
    .await;
    assert_eq!(
        first["data"]["state"], "host_key_unverified",
        "{first_text}"
    );
    let fingerprint = first["data"]["candidate_fingerprint"]
        .as_str()
        .expect("candidate fingerprint");
    let (status, _, confirmation_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/host-key-confirmations"),
        Some(json!({"fingerprint": fingerprint})),
        Some("keyboard-password-host-key-confirm"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmation_text}");
    let (status, ready, ready_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("keyboard-password-authenticated-capabilities"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ready_text}");
    assert_eq!(ready["data"]["state"], "connection_ready", "{ready_text}");
    assert_eq!(ready["data"]["capabilities"], json!(["ssh", "linux"]));
    assert!(!ready_text.contains(password));
    assert!(!database_text(&pool).await.contains(password));

    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn docker_permission_denied_after_ssh_login_keeps_connection_ready() {
    let directory = TempDir::new().expect("test directory");
    let password = "fixture-docker-permission-password-123";
    let fixture = SshFixture::start_with_password_and_docker_permission_denied(password).await;
    let (app, pool, _secret_root) = test_app(&directory).await;

    let (status, secret, secret_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(json!({"kind": "ssh_password", "password": password})),
        Some("docker-permission-password-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{secret_text}");
    assert!(!secret_text.contains(password));
    let credential_ref = secret["data"]["credential_ref"]
        .as_str()
        .expect("password credential ref");

    let (status, host, host_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/hosts",
        Some(json!({
            "display_name": "DOCKER_PERMISSION_HOST",
            "address": "127.0.0.1",
            "port": fixture.port,
            "ssh_user": "fixture",
            "credential_ref": credential_ref
        })),
        Some("docker-permission-host-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{host_text}");
    let host_id = host["data"]["host_id"].as_str().expect("host id");

    let (_, first, _) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("docker-permission-host-key-fetch"),
    )
    .await;
    assert_eq!(first["data"]["state"], "host_key_unverified");
    let fingerprint = first["data"]["candidate_fingerprint"]
        .as_str()
        .expect("candidate fingerprint");
    let (status, _, confirmation_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/host-key-confirmations"),
        Some(json!({"fingerprint": fingerprint})),
        Some("docker-permission-host-key-confirm"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmation_text}");

    let (status, ready, ready_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("docker-permission-connection-check"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ready_text}");
    assert_eq!(ready["data"]["state"], "connection_ready");
    assert!(ready["data"]["error_code"].is_null());
    assert_eq!(ready["data"]["capabilities"], json!(["ssh", "linux"]));
    assert_eq!(ready["data"]["auth_transport"], "russh_client");
    assert!(ready["data"]["error_summary"].is_null());
    assert!(!ready_text.contains(password));
    assert!(!database_text(&pool).await.contains(password));

    let (status, accepted, accepted_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({})),
        Some("docker-permission-discovery"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted_text}");
    let run_id = accepted["data"]["run_id"].as_str().expect("run id");
    let completed = wait_for_discovery(&app, run_id).await;
    assert_eq!(completed["data"]["state"], "docker_permission_denied");
    assert_eq!(
        completed["data"]["failure_code"],
        "DOCKER_PERMISSION_DENIED"
    );
    let (status, host, host_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/hosts/{host_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{host_text}");
    assert_eq!(host["data"]["status"], "connection_ready");
    assert!(host["data"]["last_error_code"].is_null());

    let (status, accepted, accepted_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({
            "provider_kinds": ["docker", "compose"],
            "requested_capabilities": ["read_only"]
        })),
        Some("explicit-docker-permission-discovery"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted_text}");
    let explicit_unavailable_run_id = accepted["data"]["run_id"]
        .as_str()
        .expect("explicit unavailable run id");
    let completed = wait_for_discovery(&app, explicit_unavailable_run_id).await;
    assert_eq!(completed["data"]["state"], "discovery_unavailable");
    assert!(completed["data"]["draft_id"].is_null());
    let (status, evidence, evidence_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/discovery-runs/{explicit_unavailable_run_id}/evidence"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{evidence_text}");
    assert!(
        evidence["data"]["provider_results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|provider| {
                provider["provider_kind"] == "docker" && provider["status"] == "permission_denied"
            })
    );
    assert!(
        evidence["data"]["provider_results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|provider| {
                provider["provider_kind"] == "compose" && provider["status"] == "unavailable"
            })
    );
    let (status, unavailable_hosts, hosts_text) =
        json_request(&app, Method::GET, "/api/v1/views/global/hosts", None, None).await;
    assert_eq!(status, StatusCode::OK, "{hosts_text}");
    let unavailable_host = &unavailable_hosts["data"]["hosts"][0];
    assert_eq!(
        unavailable_host["latest_discovery_run_id"],
        explicit_unavailable_run_id
    );
    assert_eq!(
        unavailable_host["latest_evidence_run_id"],
        explicit_unavailable_run_id
    );
    assert!(unavailable_host["latest_projection_draft_id"].is_null());

    let (status, accepted, accepted_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({
            "provider_kinds": ["docker", "compose", "systemd"],
            "requested_capabilities": ["read_only"]
        })),
        Some("docker-permission-systemd-discovery"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted_text}");
    let mixed_run_id = accepted["data"]["run_id"].as_str().expect("run id");
    let completed = wait_for_discovery(&app, mixed_run_id).await;
    assert_eq!(completed["data"]["state"], "discovery_partial");
    let mixed_draft_id = completed["data"]["draft_id"]
        .as_str()
        .expect("mixed projection draft")
        .to_owned();
    let (status, evidence, evidence_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/discovery-runs/{mixed_run_id}/evidence"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{evidence_text}");
    assert_eq!(
        evidence["data"]["systemd_units"].as_array().unwrap().len(),
        2
    );
    assert!(
        evidence["data"]["provider_results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|provider| {
                provider["provider_kind"] == "docker" && provider["status"] == "permission_denied"
            })
    );
    assert!(
        evidence["data"]["provider_results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|provider| {
                provider["provider_kind"] == "systemd" && provider["status"] == "ready"
            })
    );
    let (status, hosts_view, hosts_text) =
        json_request(&app, Method::GET, "/api/v1/views/global/hosts", None, None).await;
    assert_eq!(status, StatusCode::OK, "{hosts_text}");
    assert_eq!(hosts_view["data"]["connection_ready_count"], 1);
    assert_eq!(hosts_view["data"]["discovery_partial_count"], 1);
    assert_eq!(
        hosts_view["data"]["hosts"][0]["connection_state"],
        "connection_ready"
    );
    assert_eq!(
        hosts_view["data"]["hosts"][0]["discovery_state"],
        "discovery_partial"
    );
    assert_eq!(
        hosts_view["data"]["hosts"][0]["latest_discovery_run_id"],
        mixed_run_id
    );
    assert_eq!(
        hosts_view["data"]["hosts"][0]["latest_evidence_run_id"],
        mixed_run_id
    );
    assert_eq!(
        hosts_view["data"]["hosts"][0]["latest_projection_draft_id"],
        mixed_draft_id
    );
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn docker_unavailable_after_ssh_login_is_a_scan_failure_not_a_connection_failure() {
    let directory = TempDir::new().expect("test directory");
    let password = "fixture-docker-unavailable-password-123";
    let fixture = SshFixture::start_with_password_and_docker_unavailable(password).await;
    let (app, pool, _secret_root) = test_app(&directory).await;

    let (status, secret, secret_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(json!({"kind": "ssh_password", "password": password})),
        Some("docker-unavailable-password-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{secret_text}");
    let credential_ref = secret["data"]["credential_ref"]
        .as_str()
        .expect("password credential ref");

    let (status, host, host_text) = json_request(
        &app,
        Method::POST,
        "/api/v1/hosts",
        Some(json!({
            "display_name": "DOCKER_UNAVAILABLE_HOST",
            "address": "127.0.0.1",
            "port": fixture.port,
            "ssh_user": "fixture",
            "credential_ref": credential_ref
        })),
        Some("docker-unavailable-host-create"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{host_text}");
    let host_id = host["data"]["host_id"].as_str().expect("host id");

    let (_, first, _) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("docker-unavailable-host-key-fetch"),
    )
    .await;
    let fingerprint = first["data"]["candidate_fingerprint"]
        .as_str()
        .expect("candidate fingerprint");
    let (status, _, confirmation_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/host-key-confirmations"),
        Some(json!({"fingerprint": fingerprint})),
        Some("docker-unavailable-host-key-confirm"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmation_text}");

    let (status, capability, capability_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/connection-tests"),
        None,
        Some("docker-unavailable-connection-check"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{capability_text}");
    assert_eq!(capability["data"]["state"], "connection_ready");
    assert!(capability["data"]["error_code"].is_null());
    assert_eq!(capability["data"]["capabilities"], json!(["ssh", "linux"]));
    assert_eq!(capability["data"]["auth_transport"], "russh_client");
    assert!(!capability_text.contains(password));
    assert!(!database_text(&pool).await.contains(password));

    let (status, accepted, accepted_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({})),
        Some("docker-unavailable-discovery"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted_text}");
    assert_eq!(accepted["data"]["state"], "accepted");
    let run_id = accepted["data"]["run_id"].as_str().expect("run id");
    let completed = wait_for_discovery(&app, run_id).await;
    assert_eq!(completed["data"]["state"], "docker_unavailable");
    assert_eq!(completed["data"]["failure_code"], "DOCKER_UNAVAILABLE");
    let (status, host, host_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/hosts/{host_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{host_text}");
    assert_eq!(host["data"]["status"], "connection_ready");
    assert!(host["data"]["last_error_code"].is_null());

    let (status, accepted, accepted_text) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/hosts/{host_id}/discovery-runs"),
        Some(json!({
            "provider_kinds": ["systemd"],
            "requested_capabilities": ["read_only"]
        })),
        Some("systemd-only-discovery"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted_text}");
    let systemd_run_id = accepted["data"]["run_id"].as_str().expect("run id");
    let completed = wait_for_discovery(&app, systemd_run_id).await;
    assert_eq!(completed["data"]["state"], "discovery_complete");
    let draft_id = completed["data"]["draft_id"]
        .as_str()
        .expect("systemd evidence produces a draft");

    let (status, evidence, evidence_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/discovery-runs/{systemd_run_id}/evidence"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{evidence_text}");
    assert_eq!(
        evidence["data"]["systemd_units"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        evidence["data"]["provider_results"][0]["provider_kind"],
        "systemd"
    );
    assert_eq!(evidence["data"]["provider_results"][0]["status"], "ready");

    let (status, draft, draft_text) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/projection-drafts/{draft_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{draft_text}");
    assert!(
        draft["data"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node["kind"] != "project")
    );

    let (status, hosts_view, hosts_text) =
        json_request(&app, Method::GET, "/api/v1/views/global/hosts", None, None).await;
    assert_eq!(status, StatusCode::OK, "{hosts_text}");
    assert_eq!(
        hosts_view["data"]["hosts"][0]["connection_state"],
        "connection_ready"
    );
    assert_eq!(
        hosts_view["data"]["hosts"][0]["discovery_state"],
        "discovery_complete"
    );
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authentication_rejection_and_unused_port_have_stable_failure_classes() {
    let directory = TempDir::new().expect("test directory");
    let fixture = SshFixture::start(None, false).await;
    let (app, _pool, _secret_root) = test_app(&directory).await;
    let (_key_path, private_key) = generate_client_key(directory.path());
    let (_credential_ref, _host_id, _fingerprint, capabilities, rejected) =
        register_host(&app, &private_key, fixture.port).await;
    assert!(
        capabilities.is_empty(),
        "rejected auth must not expose capabilities"
    );
    assert_eq!(rejected["data"]["state"], "failed");
    assert_eq!(rejected["data"]["error_code"], "SSH_AUTH_FAILED");
    fixture.stop().await;

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("unused port probe");
    let unused_port = listener.local_addr().expect("unused address").port();
    drop(listener);
    let ssh = SystemSsh::new(
        ssh_binary(),
        keyscan_binary(),
        directory.path().join("unused-ssh-state"),
        SshLimits {
            connect_timeout: Duration::from_secs(1),
            command_timeout: Duration::from_secs(1),
            max_stdout_bytes: 64 * 1024,
            max_stderr_bytes: 64 * 1024,
        },
    );
    let error = ssh
        .scan_host_key("127.0.0.1", unused_port)
        .await
        .expect_err("unused port must fail");
    assert_eq!(error.failure, SshFailure::Unreachable);
}

#[tokio::test]
async fn host_alias_patch_is_idempotent_and_keeps_connection_facts() {
    let directory = TempDir::new().expect("test directory");
    let (app, pool, _secret_root) = test_app(&directory).await;
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-local', '2026-08-12T00:00:00Z')
         ON CONFLICT(workspace_id) DO NOTHING",
    )
    .execute(&pool)
    .await
    .expect("workspace");
    sqlx::query(
        "INSERT INTO hosts(
            host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
            host_key_state, transport, os, status, created_at
         ) VALUES ('host-alias', 'workspace-default', '旧别名', '192.0.2.10', 22,
            'root', 'secret://ssh/alias', 'unverified', 'ssh', 'linux',
            'host_registered', '2026-08-12T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("host");

    let uri = "/api/v1/hosts/host-alias";
    let body = json!({"display_name": "生产节点 K"});
    let (status, updated, text) = json_request(
        &app,
        Method::PATCH,
        uri,
        Some(body.clone()),
        Some("alias-update"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(updated["data"]["display_name"], "生产节点 K");
    assert_eq!(updated["data"]["address"], "192.0.2.10");
    assert_eq!(updated["data"]["port"], 22);

    let (status, replay, text) =
        json_request(&app, Method::PATCH, uri, Some(body), Some("alias-update")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(replay["data"], updated["data"]);

    let (status, conflict, text) = json_request(
        &app,
        Method::PATCH,
        uri,
        Some(json!({"display_name": "另一个别名"})),
        Some("alias-update"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(conflict["error"]["code"], "IDEMPOTENCY_KEY_REUSED");
}

#[tokio::test]
async fn host_connection_patch_resets_fingerprint_and_accepts_a_new_secret_ref() {
    let directory = TempDir::new().expect("test directory");
    let (app, pool, _secret_root) = test_app(&directory).await;
    let (_path, private_key) = generate_client_key(directory.path());
    let (status, secret, text) = json_request(
        &app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(json!({"kind": "ssh_key", "private_key": private_key})),
        Some("connection-edit-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let credential_ref = secret["data"]["credential_ref"]
        .as_str()
        .expect("credential ref");
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-local', '2026-08-12T00:00:00Z')
         ON CONFLICT(workspace_id) DO NOTHING",
    )
    .execute(&pool)
    .await
    .expect("workspace");
    sqlx::query(
        "INSERT INTO hosts(
            host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
            host_key_fingerprint, host_key_state, transport, os, status, created_at, last_checked_at
         ) VALUES ('host-edit', 'workspace-default', '旧连接', 'old.example', 22,
            'root', 'secret://ssh/old', 'SHA256:old', 'verified', 'ssh', 'linux',
            'connection_ready', '2026-08-12T00:00:00Z', '2026-08-12T00:01:00Z')",
    )
    .execute(&pool)
    .await
    .expect("host");

    let body = json!({
        "display_name": "新连接",
        "address": "new.example",
        "port": 2222,
        "ssh_user": "deploy",
        "credential_ref": credential_ref
    });
    let (status, updated, text) = json_request(
        &app,
        Method::PATCH,
        "/api/v1/hosts/host-edit",
        Some(body),
        Some("connection-edit"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(updated["data"]["address"], "new.example");
    assert_eq!(updated["data"]["port"], 2222);
    assert_eq!(updated["data"]["ssh_user"], "deploy");
    assert_eq!(updated["data"]["credential_kind"], "ssh_key");
    assert!(updated["data"].get("credential_ref").is_none());
    assert_eq!(updated["data"]["host_key_state"], "unverified");
    assert_eq!(updated["data"]["status"], "host_registered");
    assert!(updated["data"].get("host_key_fingerprint").is_none());
    assert!(updated["data"].get("last_checked_at").is_none());

    let row = sqlx::query(
        "SELECT address, port, ssh_user, credential_ref, host_key_fingerprint,
                host_key_state, status, last_checked_at FROM hosts WHERE host_id = 'host-edit'",
    )
    .fetch_one(&pool)
    .await
    .expect("updated host");
    assert_eq!(row.get::<String, _>("address"), "new.example");
    assert_eq!(row.get::<i64, _>("port"), 2222);
    assert_eq!(row.get::<String, _>("ssh_user"), "deploy");
    assert_eq!(row.get::<String, _>("credential_ref"), credential_ref);
    assert!(
        row.get::<Option<String>, _>("host_key_fingerprint")
            .is_none()
    );
    assert_eq!(row.get::<String, _>("host_key_state"), "unverified");
    assert_eq!(row.get::<String, _>("status"), "host_registered");
    assert!(row.get::<Option<String>, _>("last_checked_at").is_none());
}

#[tokio::test]
async fn m1_rejections_keep_the_unified_error_envelope() {
    let directory = TempDir::new().expect("test directory");
    let (app, _pool, _secret_root) = test_app(&directory).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/secret-refs")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{"))
                .expect("invalid JSON request"),
        )
        .await
        .expect("invalid JSON response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload: Value = serde_json::from_slice(
        &response
            .into_body()
            .collect()
            .await
            .expect("error body")
            .to_bytes(),
    )
    .expect("error JSON");
    assert_eq!(payload["error"]["code"], "INVALID_JSON");
    assert!(payload["error"]["request_id"].is_string());

    let (status, missing_key, text) = json_request(
        &app,
        Method::POST,
        "/api/v1/hosts/not-found/connection-tests",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(missing_key["error"]["code"], "IDEMPOTENCY_KEY_REQUIRED");
}
