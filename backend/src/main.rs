use std::{
    env,
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use network_atlas::{
    api, auth, m1, monitoring_api, monitoring_rollup, monitoring_scheduler, storage,
};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    if env::var("NETWORK_ATLAS_SSH_ASKPASS_MODE").as_deref() == Ok("1") {
        print_ssh_askpass_password()?;
        return Ok(());
    }
    if env::args_os().any(|argument| argument == "--hash-password") {
        print_password_hash()?;
        return Ok(());
    }
    if let Some(path) = export_openapi_path() {
        api::export_openapi(&path)?;
        println!("OPENAPI_EXPORTED {}", path.display());
        return Ok(());
    }
    if let Some(path) = argument_path("--verify-backup") {
        storage::verify_database_file(&path).await?;
        println!("BACKUP_VERIFIED {}", path.display());
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "network_atlas=info".into()),
        )
        .init();

    let bind = env::var("NETWORK_ATLAS_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_owned());
    let auth = auth::AuthConfig::from_environment(&bind)
        .context("invalid single-owner authentication configuration")?;
    let auth_enabled = auth.enabled();
    let database_url = env::var("NETWORK_ATLAS_DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://data/network-atlas.db?mode=rwc".to_owned());
    let pool = storage::connect(&database_url).await?;
    let recovered = m1::recover_interrupted_discoveries(&pool).await?;
    if recovered > 0 {
        warn!(recovered, "marked interrupted discovery runs for retry");
    }
    let recovered_monitors = monitoring_api::recover_interrupted_monitor_runs(&pool).await?;
    if recovered_monitors > 0 {
        warn!(
            recovered = recovered_monitors,
            "marked interrupted monitor runs terminal"
        );
    }
    let cleared_leases = monitoring_scheduler::clear_scheduler_leases(&pool).await?;
    if cleared_leases > 0 {
        warn!(cleared_leases, "cleared stale monitoring scheduler leases");
    }
    let interrupted_compactions = monitoring_rollup::recover_interrupted_compactions(&pool).await?;
    if interrupted_compactions > 0 {
        warn!(
            interrupted_compactions,
            "marked interrupted monitoring compactions terminal"
        );
    }
    let state = api::AppState::with_auth(pool, auth::AuthService::new(auth));
    let frontend = env::var_os("NETWORK_ATLAS_FRONTEND_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../frontend"));
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind {bind}"))?;

    if !auth_enabled {
        warn!(
            bind,
            "owner authentication is disabled for loopback development"
        );
    }
    info!(bind, auth_enabled, frontend = %frontend.display(), "network atlas listening");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let scheduler = monitoring_scheduler::spawn(state.clone(), shutdown_rx.clone());
    let compactor = monitoring_rollup::spawn(state.clone(), shutdown_rx);
    let signal_state = state.clone();
    let signal_tx = shutdown_tx.clone();
    let served = axum::serve(listener, api::router(state.clone(), frontend))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Stop scheduler claims as soon as shutdown starts, rather than
            // waiting for HTTP/SSE graceful drain to finish.
            signal_state.begin_shutdown();
            let _ = signal_tx.send(true);
        })
        .await;
    state.begin_shutdown();
    let _ = shutdown_tx.send(true);
    if let Err(error) = scheduler.await {
        warn!(error = %error, "monitoring scheduler task terminated unexpectedly");
    }
    if let Err(error) = compactor.await {
        warn!(error = %error, "monitoring compactor task terminated unexpectedly");
    }
    served?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if let Err(error) = result {
                            warn!(error = %error, "could not install Ctrl+C signal handler");
                        }
                    }
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                warn!(error = %error, "could not install SIGTERM signal handler");
                if let Err(error) = tokio::signal::ctrl_c().await {
                    warn!(error = %error, "could not install Ctrl+C signal handler");
                }
            }
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(error = %error, "could not install Ctrl+C signal handler");
    }
}

fn print_ssh_askpass_password() -> Result<()> {
    let password =
        env::var("NETWORK_ATLAS_SSH_PASSWORD").context("SSH askpass password is unavailable")?;
    write_ssh_askpass_password(std::io::stdout().lock(), &password)
}

fn write_ssh_askpass_password(mut writer: impl Write, password: &str) -> Result<()> {
    writer
        .write_all(password.as_bytes())
        .context("failed to write SSH askpass response")?;
    writer
        .flush()
        .context("failed to flush SSH askpass response")?;
    Ok(())
}

fn export_openapi_path() -> Option<PathBuf> {
    argument_path("--export-openapi")
}

fn argument_path(name: &str) -> Option<PathBuf> {
    let mut args = env::args_os().skip(1);
    while let Some(argument) = args.next() {
        if argument == name {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

fn print_password_hash() -> Result<()> {
    let mut password = String::new();
    std::io::stdin()
        .read_to_string(&mut password)
        .context("failed to read password from stdin")?;
    while password.ends_with(['\r', '\n']) {
        password.pop();
    }
    if password.is_empty() || password.len() > 1024 || password.contains('\0') {
        bail!("password must contain 1 to 1024 bytes without NUL");
    }
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|error| anyhow::anyhow!("failed to generate password salt: {error}"))?;
    let encoded = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|error| anyhow::anyhow!("failed to hash password: {error}"))?;
    println!("{encoded}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_askpass_response_is_exact_and_has_no_newline() {
        let mut output = Vec::new();
        write_ssh_askpass_password(&mut output, "fixture-special!?value").unwrap();
        assert_eq!(output, b"fixture-special!?value");
    }
}
