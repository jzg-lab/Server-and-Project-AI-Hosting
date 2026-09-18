//! Local-only SSH fixture for the M2 browser verification.

use std::{env, sync::Arc, time::Duration};

use network_atlas::discovery::{
    COMPOSE_LS_COMMAND, CONTAINERS_COMMAND, DOCKER_VERSION_COMMAND, IMAGES_COMMAND,
    LINUX_IDENTITY_COMMAND, NETWORKS_COMMAND, VOLUMES_COMMAND,
};
use rand::rng;
use russh::{
    Channel, ChannelId,
    keys::{Algorithm, EcdsaCurve, PrivateKey, ssh_key},
    server::{self, Msg, Server as _, Session},
};
use tokio::{net::TcpListener, signal};

#[derive(Clone)]
struct FixtureServer;

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
        Ok(server::Auth::Accept)
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data);
        let (stdout, stderr, exit_status) = fixture_command(&command);
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

fn fixture_command(command: &str) -> (String, String, u32) {
    if command == LINUX_IDENTITY_COMMAND {
        return (
            "Linux\n6.8.0-fixture\nubuntu\n22.04\n".to_owned(),
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
        return (r#"[{"Name":"fixture-stack","Status":"running(1)","ConfigFiles":"/srv/fixture/compose.yml"}]
"#.to_owned(), String::new(), 0);
    }
    if command == CONTAINERS_COMMAND {
        return (r#"{"id":"container-123","name":"fixture-api","image":"fixture/api:1","state":"running","status":"Up 5 minutes (healthy)","ports":"0.0.0.0:8080->8080/tcp","networks":"fixture-net","mounts":"fixture-data","created_at":"2026-08-11 00:00:00 +0000 UTC","compose_project":"fixture-stack","compose_service":"api","compose_working_dir":"/srv/fixture"}
"#.to_owned(), String::new(), 0);
    }
    if command == IMAGES_COMMAND {
        return (r#"{"id":"sha256:image-123","repository":"fixture/api","tag":"1","digest":"sha256:digest","created_at":"2026-08-10","size":"42MB"}
"#.to_owned(), String::new(), 0);
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
            "/srv/fixture/README.md\n/srv/fixture/docs/design.md\n/srv/fixture/compose.yml\n"
                .to_owned(),
            String::new(),
            0,
        );
    }
    if command.contains("/srv/fixture/README.md") {
        let body = "# Fixture project\nThis is a local browser verification fixture.\n";
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
        let body = "name: fixture-stack\nservices:\n  api:\n    image: fixture/api:1\n    ports: [\"8080:8080\"]\n    networks: [fixture-net]\n    volumes: [\"fixture-data:/data\"]\nnetworks:\n  fixture-net: {}\nvolumes:\n  fixture-data: {}\n";
        return (
            format!("{}\n{}\n{}", body.len(), "c".repeat(64), body),
            String::new(),
            0,
        );
    }
    (String::new(), "unsupported fixture command".to_owned(), 127)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut port = 22222u16;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--port" {
            port = args.next().ok_or("missing --port value")?.parse()?;
        }
    }
    let host_key = PrivateKey::random(
        &mut rng(),
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        },
    )?;
    let config = Arc::new(server::Config {
        inactivity_timeout: Some(Duration::from_secs(60)),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![host_key],
        ..Default::default()
    });
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let actual_port = listener.local_addr()?.port();
    println!("M2_SSH_FIXTURE_READY port={actual_port}");
    let mut server = FixtureServer;
    let running = server.run_on_socket(config, &listener);
    tokio::select! {
        result = running => { result?; }
        _ = signal::ctrl_c() => {}
    }
    Ok(())
}
