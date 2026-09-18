use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use russh::{
    ChannelMsg, MethodKind,
    client::{self, AuthResult, KeyboardInteractiveAuthResponse},
    keys::ssh_key,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SshLimits {
    pub connect_timeout: Duration,
    pub command_timeout: Duration,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

impl Default for SshLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            command_timeout: Duration::from_secs(15),
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SystemSsh {
    ssh_binary: PathBuf,
    keyscan_binary: PathBuf,
    state_dir: PathBuf,
    limits: SshLimits,
}

#[derive(Debug, Clone)]
pub struct SshTarget {
    pub host_id: String,
    pub address: String,
    pub port: u16,
    pub user: String,
    pub credential: SshCredential,
}

#[derive(Clone)]
pub enum SshCredential {
    PrivateKey(PathBuf),
    Password(String),
}

impl fmt::Debug for SshCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PrivateKey(path) => formatter.debug_tuple("PrivateKey").field(path).finish(),
            Self::Password(_) => formatter.write_str("Password([REDACTED])"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedHostKey {
    pub fingerprint: String,
    pub algorithm: String,
    pub known_hosts_line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr_summary: Option<String>,
    pub exit_code: i32,
    pub output_bytes: usize,
    pub auth_transport: SshAuthTransport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAuthTransport {
    RusshClient,
    OpensshAskpass,
    OpensshKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshFailure {
    Unreachable,
    Authentication,
    HostKey,
    Timeout,
    OutputLimit,
    Process,
}

#[derive(Debug, Error)]
#[error("SSH operation failed: {failure:?}")]
pub struct SshError {
    pub failure: SshFailure,
    pub summary: String,
    pub exit_code: Option<i32>,
    pub output_bytes: usize,
    pub auth_transport: Option<SshAuthTransport>,
}

impl SystemSsh {
    pub fn new(
        ssh_binary: impl Into<PathBuf>,
        keyscan_binary: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
        limits: SshLimits,
    ) -> Self {
        Self {
            ssh_binary: ssh_binary.into(),
            keyscan_binary: keyscan_binary.into(),
            state_dir: state_dir.into(),
            limits,
        }
    }

    pub fn system_default(state_dir: impl Into<PathBuf>) -> Self {
        Self::new("ssh", "ssh-keyscan", state_dir, SshLimits::default())
    }

    pub fn limits(&self) -> &SshLimits {
        &self.limits
    }

    pub async fn scan_host_key(
        &self,
        address: &str,
        port: u16,
    ) -> Result<ScannedHostKey, SshError> {
        let seconds = self.limits.connect_timeout.as_secs().max(1).to_string();
        let arguments = [
            OsString::from("-T"),
            OsString::from(seconds),
            OsString::from("-p"),
            OsString::from(port.to_string()),
            OsString::from(address),
        ];
        let output = run_limited(
            &self.keyscan_binary,
            &arguments,
            self.limits.connect_timeout + Duration::from_secs(2),
            64 * 1024,
            self.limits.max_stderr_bytes,
        )
        .await;

        match output {
            Ok(output) => {
                let stdout = String::from_utf8(output.stdout).map_err(|_| SshError {
                    failure: SshFailure::Process,
                    summary: "ssh-keyscan returned non-UTF-8 output".to_owned(),
                    exit_code: output.status_code,
                    output_bytes: output.stdout_total.saturating_add(output.stderr_total),
                    auth_transport: None,
                })?;
                if let Some(candidate) = select_host_key_line(&stdout) {
                    return fingerprint_line(candidate);
                }
                self.scan_host_key_via_client(
                    address,
                    port,
                    sanitized_summary(&output.stderr, None),
                )
                .await
            }
            Err(error) => {
                self.scan_host_key_via_client(address, port, Some(error.summary))
                    .await
            }
        }
    }

    async fn scan_host_key_via_client(
        &self,
        address: &str,
        port: u16,
        keyscan_summary: Option<String>,
    ) -> Result<ScannedHostKey, SshError> {
        self.ensure_control_files().await?;
        let candidate_path = self
            .state_dir
            .join(format!("candidate-{}.known_hosts", Uuid::new_v4()));
        let config = self.state_dir.join("empty_config");
        let global_known_hosts = self.state_dir.join("empty_global_known_hosts");
        let arguments = vec![
            OsString::from("-F"),
            config.into_os_string(),
            OsString::from("-T"),
            OsString::from("-o"),
            OsString::from("BatchMode=yes"),
            OsString::from("-o"),
            OsString::from("StrictHostKeyChecking=accept-new"),
            OsString::from("-o"),
            OsString::from(format!(
                "UserKnownHostsFile={}",
                candidate_path.to_string_lossy()
            )),
            OsString::from("-o"),
            OsString::from(format!(
                "GlobalKnownHostsFile={}",
                global_known_hosts.to_string_lossy()
            )),
            OsString::from("-o"),
            OsString::from("HashKnownHosts=no"),
            OsString::from("-o"),
            OsString::from("PubkeyAuthentication=no"),
            OsString::from("-o"),
            OsString::from("PasswordAuthentication=no"),
            OsString::from("-o"),
            OsString::from("KbdInteractiveAuthentication=no"),
            OsString::from("-o"),
            OsString::from("GSSAPIAuthentication=no"),
            OsString::from("-o"),
            OsString::from("HostbasedAuthentication=no"),
            OsString::from("-o"),
            OsString::from("CheckHostIP=no"),
            OsString::from("-o"),
            OsString::from("UpdateHostKeys=no"),
            OsString::from("-o"),
            OsString::from("VerifyHostKeyDNS=no"),
            OsString::from("-o"),
            OsString::from("ClearAllForwardings=yes"),
            OsString::from("-o"),
            OsString::from("RequestTTY=no"),
            OsString::from("-o"),
            OsString::from("LogLevel=ERROR"),
            OsString::from("-o"),
            OsString::from(format!(
                "ConnectTimeout={}",
                self.limits.connect_timeout.as_secs().max(1)
            )),
            OsString::from("-o"),
            OsString::from("ConnectionAttempts=1"),
            OsString::from("-p"),
            OsString::from(port.to_string()),
            OsString::from("--"),
            OsString::from(address),
            OsString::from("true"),
        ];
        let fallback = run_limited(
            &self.ssh_binary,
            &arguments,
            self.limits.connect_timeout + Duration::from_secs(2),
            64 * 1024,
            self.limits.max_stderr_bytes,
        )
        .await;
        let candidate = fs::read_to_string(&candidate_path).await.ok();
        let _ = fs::remove_file(&candidate_path).await;
        if let Some(candidate) = candidate
            && let Some(line) = select_host_key_line(&candidate)
        {
            return fingerprint_line(line);
        }
        match fallback {
            Err(error) => Err(error),
            Ok(output) => Err(SshError {
                failure: SshFailure::Unreachable,
                summary: sanitized_summary(&output.stderr, None)
                    .or(keyscan_summary)
                    .unwrap_or_else(|| "OpenSSH returned no supported host key".to_owned()),
                exit_code: output.status_code,
                output_bytes: output.stdout_total.saturating_add(output.stderr_total),
                auth_transport: None,
            }),
        }
    }

    pub async fn confirm_host_key(
        &self,
        host_id: &str,
        known_hosts_line: &str,
    ) -> Result<PathBuf, SshError> {
        if host_id.is_empty()
            || !host_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            || select_host_key_line(known_hosts_line).is_none()
        {
            return Err(SshError {
                failure: SshFailure::HostKey,
                summary: "invalid controlled known_hosts entry".to_owned(),
                exit_code: None,
                output_bytes: 0,
                auth_transport: None,
            });
        }
        fs::create_dir_all(&self.state_dir)
            .await
            .map_err(process_error)?;
        set_private_directory_permissions(&self.state_dir)
            .await
            .map_err(process_error)?;
        let path = self.known_hosts_path(host_id);
        let temporary = self.state_dir.join(format!("{host_id}.known_hosts.tmp"));
        let mut file = fs::File::create(&temporary).await.map_err(process_error)?;
        file.write_all(known_hosts_line.trim().as_bytes())
            .await
            .map_err(process_error)?;
        file.write_all(b"\n").await.map_err(process_error)?;
        file.flush().await.map_err(process_error)?;
        drop(file);
        set_private_file_permissions(&temporary)
            .await
            .map_err(process_error)?;
        if fs::metadata(&path).await.is_ok() {
            fs::remove_file(&path).await.map_err(process_error)?;
        }
        fs::rename(&temporary, &path).await.map_err(process_error)?;
        Ok(path)
    }

    pub async fn execute(
        &self,
        target: &SshTarget,
        command: &str,
        max_stdout_bytes: usize,
    ) -> Result<CommandOutput, SshError> {
        if let SshCredential::Password(password) = &target.credential {
            return self
                .execute_with_password(target, password, command, max_stdout_bytes)
                .await;
        }
        self.ensure_control_files().await?;
        let known_hosts = self.known_hosts_path(&target.host_id);
        if fs::metadata(&known_hosts).await.is_err() {
            return Err(SshError {
                failure: SshFailure::HostKey,
                summary: "controlled known_hosts entry is missing".to_owned(),
                exit_code: None,
                output_bytes: 0,
                auth_transport: Some(SshAuthTransport::OpensshKey),
            });
        }

        let config = self.state_dir.join("empty_config");
        let global_known_hosts = self.state_dir.join("empty_global_known_hosts");
        let target_name = format!("{}@{}", target.user, target.address);
        let arguments = vec![
            OsString::from("-F"),
            config.into_os_string(),
            OsString::from("-o"),
            OsString::from("BatchMode=yes"),
            OsString::from("-o"),
            OsString::from("IdentitiesOnly=yes"),
            OsString::from("-o"),
            OsString::from("StrictHostKeyChecking=yes"),
            OsString::from("-o"),
            OsString::from(format!(
                "UserKnownHostsFile={}",
                known_hosts.to_string_lossy()
            )),
            OsString::from("-o"),
            OsString::from(format!(
                "GlobalKnownHostsFile={}",
                global_known_hosts.to_string_lossy()
            )),
            OsString::from("-o"),
            OsString::from("PasswordAuthentication=no"),
            OsString::from("-o"),
            OsString::from("KbdInteractiveAuthentication=no"),
            OsString::from("-o"),
            OsString::from("CheckHostIP=no"),
            OsString::from("-o"),
            OsString::from("UpdateHostKeys=no"),
            OsString::from("-o"),
            OsString::from("VerifyHostKeyDNS=no"),
            OsString::from("-o"),
            OsString::from("ClearAllForwardings=yes"),
            OsString::from("-o"),
            OsString::from("RequestTTY=no"),
            OsString::from("-o"),
            OsString::from("LogLevel=ERROR"),
            OsString::from("-o"),
            OsString::from(format!(
                "ConnectTimeout={}",
                self.limits.connect_timeout.as_secs().max(1)
            )),
            OsString::from("-o"),
            OsString::from("ConnectionAttempts=1"),
            OsString::from("-p"),
            OsString::from(target.port.to_string()),
            OsString::from("-i"),
            match &target.credential {
                SshCredential::PrivateKey(path) => path.clone().into_os_string(),
                SshCredential::Password(_) => unreachable!("password handled above"),
            },
            OsString::from("--"),
            OsString::from(target_name),
            OsString::from(command),
        ];
        let raw = run_limited(
            &self.ssh_binary,
            &arguments,
            self.limits.command_timeout,
            max_stdout_bytes.min(self.limits.max_stdout_bytes),
            self.limits.max_stderr_bytes,
        )
        .await
        .map_err(|mut error| {
            let path = match &target.credential {
                SshCredential::PrivateKey(path) => Some(path.as_path()),
                SshCredential::Password(_) => None,
            };
            error.summary = sanitize_text(&error.summary, path);
            error.auth_transport = Some(SshAuthTransport::OpensshKey);
            error
        })?;

        let stdout = String::from_utf8(raw.stdout).map_err(|_| SshError {
            failure: SshFailure::Process,
            summary: "SSH command returned non-UTF-8 output".to_owned(),
            exit_code: raw.status_code,
            output_bytes: raw.stdout_total.saturating_add(raw.stderr_total),
            auth_transport: Some(SshAuthTransport::OpensshKey),
        })?;
        let credential_path = match &target.credential {
            SshCredential::PrivateKey(path) => Some(path.as_path()),
            SshCredential::Password(_) => None,
        };
        let stderr_summary = sanitized_summary(&raw.stderr, credential_path);
        let exit_code = raw.status_code.unwrap_or(-1);
        if exit_code != 0 {
            return Err(SshError {
                failure: classify_ssh_failure(exit_code, stderr_summary.as_deref()),
                summary: stderr_summary.unwrap_or_else(|| format!("SSH command exit {exit_code}")),
                exit_code: Some(exit_code),
                output_bytes: raw.stdout_total.saturating_add(raw.stderr_total),
                auth_transport: Some(SshAuthTransport::OpensshKey),
            });
        }
        Ok(CommandOutput {
            output_bytes: raw.stdout_total.saturating_add(raw.stderr_total),
            stdout,
            stderr_summary,
            exit_code,
            auth_transport: SshAuthTransport::OpensshKey,
        })
    }

    async fn execute_with_password(
        &self,
        target: &SshTarget,
        password: &str,
        command: &str,
        max_stdout_bytes: usize,
    ) -> Result<CommandOutput, SshError> {
        match self
            .execute_with_password_client(target, password, command, max_stdout_bytes)
            .await
        {
            Ok(output) => Ok(output),
            Err(error) if should_retry_password_with_system_ssh(&error) => {
                self.execute_with_password_process(target, password, command, max_stdout_bytes)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    async fn execute_with_password_client(
        &self,
        target: &SshTarget,
        password: &str,
        command: &str,
        max_stdout_bytes: usize,
    ) -> Result<CommandOutput, SshError> {
        let known_hosts = fs::read_to_string(self.known_hosts_path(&target.host_id))
            .await
            .map_err(|_| SshError {
                failure: SshFailure::HostKey,
                summary: "controlled known_hosts entry is missing".to_owned(),
                exit_code: None,
                output_bytes: 0,
                auth_transport: Some(SshAuthTransport::RusshClient),
            })?;
        let expected =
            fingerprint_line(select_host_key_line(&known_hosts).ok_or_else(|| SshError {
                failure: SshFailure::HostKey,
                summary: "controlled known_hosts entry is invalid".to_owned(),
                exit_code: None,
                output_bytes: 0,
                auth_transport: Some(SshAuthTransport::RusshClient),
            })?)?
            .fingerprint;
        let handler = PasswordClientHandler {
            expected_fingerprint: expected,
        };
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(self.limits.command_timeout),
            nodelay: true,
            ..Default::default()
        });
        let deadline = self.limits.command_timeout;
        let result = timeout(deadline, async {
            let mut session =
                client::connect(config, (target.address.as_str(), target.port), handler)
                    .await
                    .map_err(password_client_error)?;
            authenticate_password_or_keyboard_interactive(&mut session, &target.user, password)
                .await?;
            let mut channel = session
                .channel_open_session()
                .await
                .map_err(password_client_error)?;
            channel
                .exec(true, command.as_bytes())
                .await
                .map_err(password_client_error)?;
            let stdout_limit = max_stdout_bytes.min(self.limits.max_stdout_bytes);
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut exit_code = None;
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        append_password_output(&mut stdout, &data, stdout_limit, stderr.len())?
                    }
                    ChannelMsg::ExtendedData { data, .. } => append_password_output(
                        &mut stderr,
                        &data,
                        self.limits.max_stderr_bytes,
                        stdout.len(),
                    )?,
                    ChannelMsg::ExitStatus { exit_status } => {
                        exit_code = Some(i32::try_from(exit_status).unwrap_or(i32::MAX));
                    }
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            let output_bytes = stdout.len().saturating_add(stderr.len());
            let stderr_summary = sanitized_summary(&stderr, None);
            let exit_code = exit_code.unwrap_or(-1);
            if exit_code != 0 {
                return Err(SshError {
                    failure: classify_ssh_failure(exit_code, stderr_summary.as_deref()),
                    summary: stderr_summary
                        .unwrap_or_else(|| format!("SSH command exited with status {exit_code}")),
                    exit_code: Some(exit_code),
                    output_bytes,
                    auth_transport: Some(SshAuthTransport::RusshClient),
                });
            }
            let stdout = String::from_utf8(stdout).map_err(|_| SshError {
                failure: SshFailure::Process,
                summary: "SSH command returned non-UTF-8 output".to_owned(),
                exit_code: Some(exit_code),
                output_bytes,
                auth_transport: Some(SshAuthTransport::RusshClient),
            })?;
            Ok(CommandOutput {
                stdout,
                stderr_summary,
                exit_code,
                output_bytes,
                auth_transport: SshAuthTransport::RusshClient,
            })
        })
        .await;
        match result {
            Ok(value) => value,
            Err(_) => Err(SshError {
                failure: SshFailure::Timeout,
                summary: "SSH password operation timed out".to_owned(),
                exit_code: None,
                output_bytes: 0,
                auth_transport: Some(SshAuthTransport::RusshClient),
            }),
        }
    }

    async fn execute_with_password_process(
        &self,
        target: &SshTarget,
        password: &str,
        command: &str,
        max_stdout_bytes: usize,
    ) -> Result<CommandOutput, SshError> {
        self.ensure_control_files().await?;
        let known_hosts_path = self.known_hosts_path(&target.host_id);
        let known_hosts = fs::read_to_string(&known_hosts_path)
            .await
            .map_err(|_| SshError {
                failure: SshFailure::HostKey,
                summary: "controlled known_hosts entry is missing".to_owned(),
                exit_code: None,
                output_bytes: 0,
                auth_transport: Some(SshAuthTransport::OpensshAskpass),
            })?;
        let Some(candidate) = select_host_key_line(&known_hosts) else {
            return Err(SshError {
                failure: SshFailure::HostKey,
                summary: "controlled known_hosts entry is invalid".to_owned(),
                exit_code: None,
                output_bytes: 0,
                auth_transport: Some(SshAuthTransport::OpensshAskpass),
            });
        };
        let _ = fingerprint_line(candidate)?;

        let askpass_path = std::env::current_exe().map_err(process_error)?;
        let config = self.state_dir.join("empty_config");
        let global_known_hosts = self.state_dir.join("empty_global_known_hosts");
        let target_name = format!("{}@{}", target.user, target.address);
        let arguments = vec![
            OsString::from("-F"),
            config.into_os_string(),
            OsString::from("-o"),
            OsString::from("BatchMode=no"),
            OsString::from("-o"),
            OsString::from("NumberOfPasswordPrompts=1"),
            OsString::from("-o"),
            OsString::from("PubkeyAuthentication=no"),
            OsString::from("-o"),
            OsString::from("PreferredAuthentications=password,keyboard-interactive"),
            OsString::from("-o"),
            OsString::from("IdentitiesOnly=yes"),
            OsString::from("-o"),
            OsString::from("StrictHostKeyChecking=yes"),
            OsString::from("-o"),
            OsString::from(format!(
                "UserKnownHostsFile={}",
                known_hosts_path.to_string_lossy()
            )),
            OsString::from("-o"),
            OsString::from(format!(
                "GlobalKnownHostsFile={}",
                global_known_hosts.to_string_lossy()
            )),
            OsString::from("-o"),
            OsString::from("CheckHostIP=no"),
            OsString::from("-o"),
            OsString::from("UpdateHostKeys=no"),
            OsString::from("-o"),
            OsString::from("VerifyHostKeyDNS=no"),
            OsString::from("-o"),
            OsString::from("ClearAllForwardings=yes"),
            OsString::from("-o"),
            OsString::from("RequestTTY=no"),
            OsString::from("-o"),
            OsString::from("LogLevel=ERROR"),
            OsString::from("-o"),
            OsString::from(format!(
                "ConnectTimeout={}",
                self.limits.connect_timeout.as_secs().max(1)
            )),
            OsString::from("-o"),
            OsString::from("ConnectionAttempts=1"),
            OsString::from("-p"),
            OsString::from(target.port.to_string()),
            OsString::from("--"),
            OsString::from(target_name),
            OsString::from(command),
        ];
        let env = HashMap::from([
            (
                "SSH_ASKPASS".to_owned(),
                askpass_path.as_os_str().to_os_string(),
            ),
            ("SSH_ASKPASS_REQUIRE".to_owned(), OsString::from("force")),
            (
                "NETWORK_ATLAS_SSH_ASKPASS_MODE".to_owned(),
                OsString::from("1"),
            ),
            (
                "NETWORK_ATLAS_SSH_PASSWORD".to_owned(),
                OsString::from(password),
            ),
            ("DISPLAY".to_owned(), OsString::from("none:0")),
        ]);
        let raw = run_limited_with_env(
            &self.ssh_binary,
            &arguments,
            &env,
            self.limits.command_timeout,
            max_stdout_bytes.min(self.limits.max_stdout_bytes),
            self.limits.max_stderr_bytes,
        )
        .await
        .map_err(|mut error| {
            error.summary = sanitize_text(&error.summary, Some(&askpass_path));
            error.auth_transport = Some(SshAuthTransport::OpensshAskpass);
            error
        });
        let raw = raw?;

        let stdout = String::from_utf8(raw.stdout).map_err(|_| SshError {
            failure: SshFailure::Process,
            summary: "SSH command returned non-UTF-8 output".to_owned(),
            exit_code: raw.status_code,
            output_bytes: raw.stdout_total.saturating_add(raw.stderr_total),
            auth_transport: Some(SshAuthTransport::OpensshAskpass),
        })?;
        let stderr_summary = sanitized_summary(&raw.stderr, Some(&askpass_path));
        let exit_code = raw.status_code.unwrap_or(-1);
        if exit_code != 0 {
            return Err(SshError {
                failure: classify_ssh_failure(exit_code, stderr_summary.as_deref()),
                summary: stderr_summary.unwrap_or_else(|| format!("SSH command exit {exit_code}")),
                exit_code: Some(exit_code),
                output_bytes: raw.stdout_total.saturating_add(raw.stderr_total),
                auth_transport: Some(SshAuthTransport::OpensshAskpass),
            });
        }
        Ok(CommandOutput {
            output_bytes: raw.stdout_total.saturating_add(raw.stderr_total),
            stdout,
            stderr_summary,
            exit_code,
            auth_transport: SshAuthTransport::OpensshAskpass,
        })
    }

    pub fn known_hosts_path(&self, host_id: &str) -> PathBuf {
        self.state_dir.join(format!("{host_id}.known_hosts"))
    }

    async fn ensure_control_files(&self) -> Result<(), SshError> {
        fs::create_dir_all(&self.state_dir)
            .await
            .map_err(process_error)?;
        set_private_directory_permissions(&self.state_dir)
            .await
            .map_err(process_error)?;
        for (name, content) in [
            ("empty_config", "Host *\n    PermitLocalCommand no\n"),
            ("empty_global_known_hosts", ""),
        ] {
            let path = self.state_dir.join(name);
            if fs::metadata(&path).await.is_err() {
                fs::write(&path, content).await.map_err(process_error)?;
                set_private_file_permissions(&path)
                    .await
                    .map_err(process_error)?;
            }
        }
        Ok(())
    }
}

async fn authenticate_password_or_keyboard_interactive(
    session: &mut client::Handle<PasswordClientHandler>,
    user: &str,
    password: &str,
) -> Result<(), SshError> {
    let authenticated = session
        .authenticate_password(user, password)
        .await
        .map_err(password_client_error)?;
    match authenticated {
        AuthResult::Success => Ok(()),
        AuthResult::Failure {
            remaining_methods,
            partial_success,
        } => {
            if partial_success {
                return Err(password_authentication_error(
                    "SSH password was accepted but the server requested an additional authentication factor",
                ));
            }
            if remaining_methods.contains(&MethodKind::KeyboardInteractive) {
                authenticate_single_password_keyboard_interactive(session, user, password).await
            } else {
                Err(password_authentication_error(
                    "SSH password authentication was rejected",
                ))
            }
        }
    }
}

async fn authenticate_single_password_keyboard_interactive(
    session: &mut client::Handle<PasswordClientHandler>,
    user: &str,
    password: &str,
) -> Result<(), SshError> {
    let mut response = session
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(password_client_error)?;
    for _ in 0..4 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure {
                partial_success, ..
            } => {
                return if partial_success {
                    Err(password_authentication_error(
                        "SSH keyboard-interactive password was accepted but the server requested an additional authentication factor",
                    ))
                } else {
                    Err(password_authentication_error(
                        "SSH keyboard-interactive password authentication was rejected",
                    ))
                };
            }
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers = if prompts.is_empty() {
                    Vec::new()
                } else if prompts.len() == 1 && !prompts[0].echo {
                    vec![password.to_owned()]
                } else {
                    return Err(password_authentication_error(
                        "SSH keyboard-interactive requested prompts that MVP-1 does not automate",
                    ));
                };
                response = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(password_client_error)?;
            }
        }
    }
    Err(password_authentication_error(
        "SSH keyboard-interactive authentication did not finish within the prompt limit",
    ))
}

fn password_authentication_error(summary: &str) -> SshError {
    SshError {
        failure: SshFailure::Authentication,
        summary: summary.to_owned(),
        exit_code: None,
        output_bytes: 0,
        auth_transport: Some(SshAuthTransport::RusshClient),
    }
}

#[derive(Debug)]
struct PasswordClientHandler {
    expected_fingerprint: String,
}

impl client::Handler for PasswordClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(server_public_key
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string()
            == self.expected_fingerprint)
    }
}

fn password_client_error(error: russh::Error) -> SshError {
    let summary = error.to_string();
    let lower = summary.to_ascii_lowercase();
    let failure = match &error {
        russh::Error::UnknownKey
        | russh::Error::WrongServerSig
        | russh::Error::KeyChanged { .. }
        | russh::Error::PacketAuth => SshFailure::HostKey,
        russh::Error::NotAuthenticated
        | russh::Error::UnsupportedAuthMethod
        | russh::Error::NoAuthMethod => SshFailure::Authentication,
        russh::Error::ConnectionTimeout
        | russh::Error::KeepaliveTimeout
        | russh::Error::InactivityTimeout
        | russh::Error::Elapsed(_) => SshFailure::Timeout,
        russh::Error::Disconnect | russh::Error::HUP => SshFailure::Unreachable,
        russh::Error::IO(error) => match error.kind() {
            std::io::ErrorKind::TimedOut => SshFailure::Timeout,
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::AddrNotAvailable
            | std::io::ErrorKind::NetworkUnreachable
            | std::io::ErrorKind::HostUnreachable
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::UnexpectedEof => SshFailure::Unreachable,
            _ => classify_password_client_summary(&lower),
        },
        _ => classify_password_client_summary(&lower),
    };
    SshError {
        failure,
        summary: sanitize_text(&summary, None),
        exit_code: None,
        output_bytes: 0,
        auth_transport: Some(SshAuthTransport::RusshClient),
    }
}

fn classify_password_client_summary(lower: &str) -> SshFailure {
    if lower.contains("key") || lower.contains("fingerprint") {
        SshFailure::HostKey
    } else if lower.contains("auth")
        || lower.contains("permission denied")
        || lower.contains("password")
    {
        SshFailure::Authentication
    } else if lower.contains("timed out") || lower.contains("timeout") {
        SshFailure::Timeout
    } else if lower.contains("refused")
        || lower.contains("unreachable")
        || lower.contains("resolve")
        || lower.contains("socket")
        || lower.contains("connection closed")
    {
        SshFailure::Unreachable
    } else {
        SshFailure::Process
    }
}

fn should_retry_password_with_system_ssh(error: &SshError) -> bool {
    error
        .summary
        .to_ascii_lowercase()
        .contains("wrong server signature")
}

fn append_password_output(
    output: &mut Vec<u8>,
    data: &[u8],
    limit: usize,
    other_bytes: usize,
) -> Result<(), SshError> {
    if output.len().saturating_add(data.len()) > limit {
        return Err(SshError {
            failure: SshFailure::OutputLimit,
            summary: "SSH process output limit exceeded".to_owned(),
            exit_code: None,
            output_bytes: output
                .len()
                .saturating_add(data.len())
                .saturating_add(other_bytes),
            auth_transport: Some(SshAuthTransport::RusshClient),
        });
    }
    output.extend_from_slice(data);
    Ok(())
}

#[derive(Debug)]
struct RawOutput {
    status_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_total: usize,
    stderr_total: usize,
}

struct LimitedRead {
    bytes: Vec<u8>,
    total: usize,
    exceeded: bool,
}

async fn run_limited(
    binary: &Path,
    arguments: &[impl AsRef<OsStr>],
    deadline: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<RawOutput, SshError> {
    run_limited_with_env(
        binary,
        arguments,
        &HashMap::new(),
        deadline,
        stdout_limit,
        stderr_limit,
    )
    .await
}

async fn run_limited_with_env(
    binary: &Path,
    arguments: &[impl AsRef<OsStr>],
    env: &HashMap<String, OsString>,
    deadline: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<RawOutput, SshError> {
    let mut command = Command::new(binary);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(process_error)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| process_error_message("stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| process_error_message("stderr unavailable"))?;
    let stdout_task = tokio::spawn(read_limited(stdout, stdout_limit));
    let stderr_task = tokio::spawn(read_limited(stderr, stderr_limit));

    let status = match timeout(deadline, child.wait()).await {
        Ok(result) => result.map_err(process_error)?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let stdout_total = stdout_task
                .await
                .ok()
                .and_then(Result::ok)
                .map(|output| output.total)
                .unwrap_or(0);
            let stderr_total = stderr_task
                .await
                .ok()
                .and_then(Result::ok)
                .map(|output| output.total)
                .unwrap_or(0);
            return Err(SshError {
                failure: SshFailure::Timeout,
                summary: "SSH process deadline exceeded".to_owned(),
                exit_code: None,
                output_bytes: stdout_total.saturating_add(stderr_total),
                auth_transport: None,
            });
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|_| process_error_message("stdout reader failed"))?
        .map_err(process_error)?;
    let stderr = stderr_task
        .await
        .map_err(|_| process_error_message("stderr reader failed"))?
        .map_err(process_error)?;
    if stdout.exceeded || stderr.exceeded {
        return Err(SshError {
            failure: SshFailure::OutputLimit,
            summary: "SSH process output limit exceeded".to_owned(),
            exit_code: status.code(),
            output_bytes: stdout.total.saturating_add(stderr.total),
            auth_transport: None,
        });
    }
    Ok(RawOutput {
        status_code: status.code(),
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stdout_total: stdout.total,
        stderr_total: stderr.total,
    })
}

async fn read_limited(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<LimitedRead, std::io::Error> {
    let mut stored = Vec::with_capacity(limit.min(64 * 1024));
    let mut total = 0usize;
    let mut exceeded = false;
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read);
        let remaining = limit.saturating_sub(stored.len());
        stored.extend_from_slice(&chunk[..read.min(remaining)]);
        if total > limit {
            exceeded = true;
        }
    }
    Ok(LimitedRead {
        bytes: stored,
        total,
        exceeded,
    })
}

fn select_host_key_line(output: &str) -> Option<&str> {
    let supported = ["ssh-ed25519", "ecdsa-sha2-nistp256", "ssh-rsa"];
    supported.into_iter().find_map(|algorithm| {
        output.lines().find(|line| {
            let mut fields = line.split_whitespace();
            let _host = fields.next();
            fields.next() == Some(algorithm) && fields.next().is_some() && !line.starts_with('#')
        })
    })
}

fn fingerprint_line(line: &str) -> Result<ScannedHostKey, SshError> {
    let mut fields = line.split_whitespace();
    let host = fields.next().ok_or_else(invalid_host_key)?;
    let algorithm = fields.next().ok_or_else(invalid_host_key)?;
    let encoded = fields.next().ok_or_else(invalid_host_key)?;
    if fields.next().is_some() {
        return Err(invalid_host_key());
    }
    let key = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| invalid_host_key())?;
    let digest = Sha256::digest(key);
    Ok(ScannedHostKey {
        fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode(digest)),
        algorithm: algorithm.to_owned(),
        known_hosts_line: format!("{host} {algorithm} {encoded}"),
    })
}

fn invalid_host_key() -> SshError {
    SshError {
        failure: SshFailure::HostKey,
        summary: "invalid host key line".to_owned(),
        exit_code: None,
        output_bytes: 0,
        auth_transport: None,
    }
}

fn classify_ssh_failure(exit_code: i32, summary: Option<&str>) -> SshFailure {
    let value = summary.unwrap_or_default().to_ascii_lowercase();
    if value.contains("permission denied (")
        || value.contains("authentication failed")
        || value.contains("all configured authentication methods failed")
    {
        SshFailure::Authentication
    } else if value.contains("host key verification failed")
        || value.contains("remote host identification has changed")
    {
        SshFailure::HostKey
    } else if value.contains("timed out") {
        SshFailure::Timeout
    } else if exit_code == 255
        || value.contains("connection refused")
        || value.contains("no route to host")
        || value.contains("could not resolve hostname")
        || value.contains("connection closed")
    {
        SshFailure::Unreachable
    } else {
        SshFailure::Process
    }
}

fn sanitized_summary(bytes: &[u8], credential_path: Option<&Path>) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let value = String::from_utf8_lossy(bytes);
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated = compact.chars().take(512).collect::<String>();
    if truncated.is_empty() {
        None
    } else {
        Some(sanitize_text(&truncated, credential_path))
    }
}

fn sanitize_text(value: &str, credential_path: Option<&Path>) -> String {
    let mut result = value.to_owned();
    if let Some(path) = credential_path {
        let path = path.to_string_lossy();
        result = result.replace(path.as_ref(), "<CREDENTIAL_PATH>");
        result = result.replace(&path.replace('\\', "/"), "<CREDENTIAL_PATH>");
    }
    result
}

fn process_error(error: std::io::Error) -> SshError {
    process_error_message(&format!("SSH process unavailable ({:?})", error.kind()))
}

fn process_error_message(message: &str) -> SshError {
    SshError {
        failure: SshFailure::Process,
        summary: message.to_owned(),
        exit_code: None,
        output_bytes: 0,
        auth_transport: None,
    }
}

#[cfg(unix)]
async fn set_private_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await
}

#[cfg(not(unix))]
async fn set_private_file_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
async fn set_private_directory_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await
}

#[cfg(not(unix))]
async fn set_private_directory_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_command(script: &str) -> (PathBuf, Vec<OsString>) {
        if cfg!(windows) {
            (
                PathBuf::from("powershell.exe"),
                vec![
                    OsString::from("-NoProfile"),
                    OsString::from("-Command"),
                    OsString::from(script),
                ],
            )
        } else {
            (
                PathBuf::from("/bin/sh"),
                vec![OsString::from("-c"), OsString::from(script)],
            )
        }
    }

    #[test]
    fn fingerprint_matches_openssh_sha256_shape() {
        let line =
            "host ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEvYkw8U+xxQms7QDjZ/vHOIKQrDPv3jITL8a9f8lLQx";
        let scanned = fingerprint_line(line).unwrap();
        assert!(scanned.fingerprint.starts_with("SHA256:"));
        assert!(!scanned.fingerprint.ends_with('='));
        assert_eq!(scanned.algorithm, "ssh-ed25519");
    }

    #[test]
    fn classifies_transport_failures_without_exposing_arguments() {
        assert_eq!(
            classify_ssh_failure(255, Some("Permission denied (publickey).")),
            SshFailure::Authentication
        );
        assert_eq!(
            classify_ssh_failure(
                1,
                Some("Got permission denied while trying to connect to the Docker daemon socket")
            ),
            SshFailure::Process
        );
        assert_eq!(
            classify_ssh_failure(255, Some("Host key verification failed.")),
            SshFailure::HostKey
        );
        assert_eq!(
            classify_ssh_failure(255, Some("Connection refused")),
            SshFailure::Unreachable
        );
        let sanitized = sanitize_text(
            "Load key C:/private/fixture.key failed",
            Some(Path::new(r"C:\private\fixture.key")),
        );
        assert!(!sanitized.contains("private/fixture.key"));
        assert!(sanitized.contains("<CREDENTIAL_PATH>"));
    }

    #[test]
    fn password_credential_debug_output_is_redacted() {
        let password = "debug-must-not-leak";
        let rendered = format!("{:?}", SshCredential::Password(password.to_owned()));
        assert_eq!(rendered, "Password([REDACTED])");
        assert!(!rendered.contains(password));
    }

    #[test]
    fn password_client_uses_io_error_kinds_instead_of_localized_messages() {
        let refused = password_client_error(russh::Error::IO(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused,
        )));
        assert_eq!(refused.failure, SshFailure::Unreachable);

        let timed_out = password_client_error(russh::Error::IO(std::io::Error::from(
            std::io::ErrorKind::TimedOut,
        )));
        assert_eq!(timed_out.failure, SshFailure::Timeout);
    }

    #[tokio::test]
    async fn process_deadline_is_classified_as_timeout() {
        let script = if cfg!(windows) {
            "Start-Sleep -Milliseconds 500"
        } else {
            "sleep 1"
        };
        let (binary, arguments) = shell_command(script);
        let error = run_limited(&binary, &arguments, Duration::from_millis(50), 1024, 1024)
            .await
            .expect_err("deadline must stop the process");
        assert_eq!(error.failure, SshFailure::Timeout);
    }

    #[tokio::test]
    async fn process_output_limit_is_classified_and_pipes_are_drained() {
        let script = if cfg!(windows) {
            "[Console]::Out.Write(('x' * 4096))"
        } else {
            "head -c 4096 /dev/zero | tr '\\0' x"
        };
        let (binary, arguments) = shell_command(script);
        let error = run_limited(&binary, &arguments, Duration::from_secs(2), 32, 32)
            .await
            .expect_err("oversized output must be rejected");
        assert_eq!(error.failure, SshFailure::OutputLimit);
        assert!(error.output_bytes >= 4096);
    }
}
