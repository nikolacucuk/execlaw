//! Confined one-shot workspace process and Language Server Protocol jobs.

use async_trait::async_trait;
use bollard::container::{
    AttachContainerOptions, Config, CreateContainerOptions, LogOutput, LogsOptions,
    RemoveContainerOptions, StartContainerOptions, WaitContainerOptions,
};
use bollard::secret::{HostConfig, Mount, MountBindOptions, MountTypeEnum};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

const OUTPUT_LIMIT_BYTES: usize = 128 * 1024;
const LSP_FRAME_LIMIT_BYTES: usize = 1024 * 1024;
const DIAGNOSTIC_LIMIT: usize = 512;
const JOB_MEMORY_BYTES: i64 = 2 * 1024 * 1024 * 1024;
const JOB_NANO_CPUS: i64 = 2_000_000_000;
const JOB_PID_LIMIT: i64 = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRunRequest {
    pub image_reference: String,
    pub checkout_path: PathBuf,
    pub job_name: String,
    pub argv: Vec<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRunResult {
    pub exit_code: Option<i64>,
    pub timed_out: bool,
    pub output_truncated: bool,
    pub output: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDiagnosticsRequest {
    pub image_reference: String,
    pub checkout_path: PathBuf,
    pub job_name: String,
    pub server_argv: Vec<String>,
    pub path: String,
    pub language_id: String,
    pub text: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDiagnostic {
    pub range: serde_json::Value,
    pub severity: Option<u8>,
    pub code: Option<serde_json::Value>,
    pub source: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDiagnosticsResult {
    pub language_id: String,
    pub path: String,
    pub diagnostics: Vec<WorkspaceDiagnostic>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Error)]
pub enum WorkspaceExecutionError {
    #[error("invalid workspace job: {0}")]
    Invalid(String),
    #[error("workspace toolchain provenance rejected: {0}")]
    Provenance(String),
    #[error("workspace container runtime: {0}")]
    Runtime(String),
    #[error("language server protocol: {0}")]
    Protocol(String),
}

/// Host dispatch contract for isolated checkout jobs.
#[async_trait]
pub trait WorkspaceJobExecutor: Send + Sync {
    /// Run an argv vector inside a network-disabled, resource-capped container.
    async fn run(
        &self,
        request: WorkspaceRunRequest,
    ) -> Result<WorkspaceRunResult, WorkspaceExecutionError>;

    /// Obtain push diagnostics from a configured generic LSP server.
    async fn diagnostics(
        &self,
        request: WorkspaceDiagnosticsRequest,
    ) -> Result<WorkspaceDiagnosticsResult, WorkspaceExecutionError>;
}

/// Bollard-backed executor for Controller-authorized coding checkouts.
#[derive(Clone)]
pub struct BollardWorkspaceJobExecutor {
    docker: bollard::Docker,
    provenance: execlaw_core::artifact_provenance::ArtifactProvenanceStore,
}

impl BollardWorkspaceJobExecutor {
    /// Connect to the local Docker daemon and require persisted artifact provenance.
    pub fn connect(db: execlaw_core::Database) -> Result<Self, WorkspaceExecutionError> {
        let docker = bollard::Docker::connect_with_local_defaults()
            .map_err(|error| WorkspaceExecutionError::Runtime(error.to_string()))?;
        Ok(Self {
            docker,
            provenance: execlaw_core::artifact_provenance::ArtifactProvenanceStore::new(db),
        })
    }

    /// Build a job executor around an existing Docker connection.
    pub fn with_docker(docker: bollard::Docker, db: execlaw_core::Database) -> Self {
        Self {
            docker,
            provenance: execlaw_core::artifact_provenance::ArtifactProvenanceStore::new(db),
        }
    }

    async fn authorize_image(&self, image: &str) -> Result<(), WorkspaceExecutionError> {
        self.provenance
            .authorize_oci_reference(
                "sidecar:workspace-toolchain",
                execlaw_core::artifact_provenance::ArtifactType::Sidecar,
                image,
                "workspace-job-executor",
            )
            .map_err(|error| WorkspaceExecutionError::Provenance(error.to_string()))?;
        self.docker.inspect_image(image).await.map_err(|error| {
            WorkspaceExecutionError::Runtime(format!(
                "approved workspace toolchain image is not installed locally: {error}"
            ))
        })?;
        Ok(())
    }

    async fn create_job_container(
        &self,
        image: &str,
        checkout_path: &Path,
        job_name: &str,
        command: &[String],
        timeout_ms: u64,
        open_stdin: bool,
    ) -> Result<String, WorkspaceExecutionError> {
        let checkout_path = validate_checkout(checkout_path)?;
        validate_job_name(job_name)?;
        validate_argv(command)?;
        validate_timeout(timeout_ms)?;
        self.authorize_image(image).await?;

        let name = container_name(job_name);
        // A replay after a process crash may encounter a container from the
        // same durable job. The database lease prevents removing a live job;
        // once reclaimed, its old container is safe to replace.
        let _ = self.docker.kill_container::<&str>(&name, None).await;
        let _ = self
            .docker
            .remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let seconds = timeout_ms.saturating_add(999) / 1000;
        let mut cmd = vec![
            "--signal=TERM".to_owned(),
            "--kill-after=2s".to_owned(),
            format!("{seconds}s"),
        ];
        cmd.extend(command.iter().cloned());
        let mut tmpfs = HashMap::new();
        tmpfs.insert(
            "/tmp".to_owned(),
            "rw,exec,nosuid,nodev,size=128m,mode=1777".to_owned(),
        );
        let host_config = HostConfig {
            mounts: Some(vec![Mount {
                source: Some(docker_host_path(&checkout_path)),
                target: Some("/workspace".into()),
                typ: Some(MountTypeEnum::BIND),
                read_only: Some(true),
                bind_options: Some(MountBindOptions {
                    non_recursive: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            network_mode: Some("none".into()),
            readonly_rootfs: Some(true),
            cap_drop: Some(vec!["ALL".into()]),
            security_opt: Some(vec!["no-new-privileges:true".into()]),
            memory: Some(JOB_MEMORY_BYTES),
            nano_cpus: Some(JOB_NANO_CPUS),
            pids_limit: Some(JOB_PID_LIMIT),
            tmpfs: Some(tmpfs),
            auto_remove: Some(false),
            ..Default::default()
        };
        let config = Config {
            image: Some(image.to_owned()),
            entrypoint: Some(vec!["/usr/bin/timeout".into()]),
            cmd: Some(cmd),
            working_dir: Some("/workspace".into()),
            user: Some(container_user(&checkout_path)?),
            env: Some(vec![
                "HOME=/tmp".into(),
                "TMPDIR=/tmp".into(),
                "CARGO_NET_OFFLINE=true".into(),
                "CARGO_TARGET_DIR=/tmp/target".into(),
                "NO_PROXY=*".into(),
            ]),
            attach_stdin: Some(open_stdin),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            open_stdin: Some(open_stdin),
            tty: Some(false),
            host_config: Some(host_config),
            labels: Some(HashMap::from([
                ("execlaw.kind".into(), "workspace-job".into()),
                ("execlaw.job".into(), job_name.into()),
            ])),
            ..Default::default()
        };
        self.docker
            .create_container(
                Some(CreateContainerOptions {
                    name: name.clone(),
                    platform: None,
                }),
                config,
            )
            .await
            .map_err(|error| WorkspaceExecutionError::Runtime(error.to_string()))?;
        Ok(name)
    }

    async fn wait_for_exit(
        &self,
        name: &str,
        timeout_ms: u64,
    ) -> Result<(Option<i64>, bool), WorkspaceExecutionError> {
        let mut waiter = self.docker.wait_container(
            name,
            Some(WaitContainerOptions {
                condition: "not-running".to_owned(),
            }),
        );
        let wait = tokio::time::timeout(Duration::from_millis(timeout_ms), waiter.next()).await;
        match wait {
            Ok(Some(Ok(response))) => Ok((Some(response.status_code), false)),
            Ok(Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. }))) => {
                Ok((Some(i64::from(code)), false))
            }
            Ok(Some(Err(error))) => Err(WorkspaceExecutionError::Runtime(error.to_string())),
            Ok(None) => Err(WorkspaceExecutionError::Runtime(
                "container exited without a wait result".into(),
            )),
            Err(_) => {
                let _ = self.docker.kill_container::<&str>(name, None).await;
                let mut after_kill = self.docker.wait_container(
                    name,
                    Some(WaitContainerOptions {
                        condition: "not-running".to_owned(),
                    }),
                );
                let _ = tokio::time::timeout(Duration::from_secs(5), after_kill.next()).await;
                Ok((None, true))
            }
        }
    }

    async fn read_logs(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<(String, bool), WorkspaceExecutionError> {
        let options = LogsOptions::<String> {
            follow: false,
            stdout: true,
            stderr: true,
            since: 0,
            until: 0,
            timestamps: false,
            tail: "all".into(),
        };
        let mut logs = self.docker.logs(name, Some(options));
        let mut output = Vec::new();
        let mut truncated = false;
        while let Some(item) = logs.next().await {
            let item = item.map_err(|error| WorkspaceExecutionError::Runtime(error.to_string()))?;
            let bytes = item.into_bytes();
            let remaining = limit.saturating_sub(output.len());
            if bytes.len() > remaining {
                output.extend_from_slice(&bytes[..remaining]);
                truncated = true;
            } else {
                output.extend_from_slice(&bytes);
            }
        }
        Ok((String::from_utf8_lossy(&output).into_owned(), truncated))
    }

    async fn remove_job_container(&self, name: &str) {
        let _ = self
            .docker
            .remove_container(
                name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }

    async fn diagnostics_session(
        &self,
        request: WorkspaceDiagnosticsRequest,
    ) -> Result<WorkspaceDiagnosticsResult, WorkspaceExecutionError> {
        validate_lsp_request(&request)?;
        let started = Instant::now();
        let name = self
            .create_job_container(
                &request.image_reference,
                &request.checkout_path,
                &request.job_name,
                &request.server_argv,
                request.timeout_ms,
                true,
            )
            .await?;
        let result = self.lsp_exchange(&name, &request).await;
        self.remove_job_container(&name).await;
        let diagnostics = result?;
        Ok(WorkspaceDiagnosticsResult {
            language_id: request.language_id,
            path: request.path,
            diagnostics,
            elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        })
    }

    async fn lsp_exchange(
        &self,
        name: &str,
        request: &WorkspaceDiagnosticsRequest,
    ) -> Result<Vec<WorkspaceDiagnostic>, WorkspaceExecutionError> {
        let mut attached = self
            .docker
            .attach_container(
                name,
                Some(AttachContainerOptions::<String> {
                    stdin: Some(true),
                    stdout: Some(true),
                    stderr: Some(true),
                    stream: Some(true),
                    logs: Some(false),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|error| WorkspaceExecutionError::Runtime(error.to_string()))?;
        self.docker
            .start_container(name, None::<StartContainerOptions<String>>)
            .await
            .map_err(|error| WorkspaceExecutionError::Runtime(error.to_string()))?;

        let root_uri = url::Url::parse("file:///workspace/")
            .map_err(|_| WorkspaceExecutionError::Protocol("invalid workspace URI".into()))?;
        let document_uri = root_uri
            .join(&request.path)
            .map_err(|_| WorkspaceExecutionError::Protocol("invalid document URI".into()))?
            .to_string();
        let root_uri = root_uri.to_string();
        let initialize = serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "processId":null,"rootUri":root_uri.clone(),"workspaceFolders":[{"uri":root_uri,"name":"workspace"}],
                "capabilities":{"general":{"positionEncodings":["utf-16"]},
                    "textDocument":{"publishDiagnostics":{"relatedInformation":true}}},
                "clientInfo":{"name":"execlaw","version":env!("CARGO_PKG_VERSION")}
            }
        });
        send_lsp_message(&mut attached.input, &initialize).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms);
        let mut frames = LspFrameDecoder::default();
        let mut initialized = false;
        let mut supports_pull_diagnostics = false;
        let mut stderr = Vec::new();
        while !initialized {
            let message =
                next_lsp_message(&mut attached.output, &mut frames, deadline, &mut stderr).await?;
            if message.get("id") == Some(&serde_json::json!(1)) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(WorkspaceExecutionError::Protocol(format!(
                        "language server initialization failed: {}",
                        bounded_json(error, 1024)
                    )));
                }
                supports_pull_diagnostics = message
                    .pointer("/result/capabilities/diagnosticProvider")
                    .is_some_and(serde_json::Value::is_object);
                initialized = true;
            } else if message.get("method").is_some() && message.get("id").is_some() {
                answer_lsp_request(&mut attached.input, &message).await?;
            }
        }
        send_lsp_message(
            &mut attached.input,
            &serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        )
        .await?;
        send_lsp_message(
            &mut attached.input,
            &serde_json::json!({
                "jsonrpc":"2.0","method":"textDocument/didOpen","params":{
                    "textDocument":{"uri":document_uri.clone(),"languageId":request.language_id,
                        "version":1,"text":request.text}
                }
            }),
        )
        .await?;
        if supports_pull_diagnostics {
            send_lsp_message(
                &mut attached.input,
                &serde_json::json!({
                    "jsonrpc":"2.0","id":2,"method":"textDocument/diagnostic","params":{
                        "textDocument":{"uri":document_uri.clone()},"identifier":null,"previousResultId":null
                    }
                }),
            )
            .await?;
        }

        let mut empty_push_report = None;
        let mut empty_push_settle_deadline: Option<tokio::time::Instant> = None;
        let diagnostics = loop {
            let message_deadline =
                empty_push_settle_deadline.map_or(deadline, |settle| settle.min(deadline));
            let message = match next_lsp_message(
                &mut attached.output,
                &mut frames,
                message_deadline,
                &mut stderr,
            )
            .await
            {
                Ok(message) => message,
                Err(error)
                    if empty_push_report.is_some()
                        && empty_push_settle_deadline
                            .is_some_and(|settle| tokio::time::Instant::now() >= settle) =>
                {
                    let _ = error;
                    break empty_push_report.take().unwrap_or_default();
                }
                Err(error) => return Err(error),
            };
            if message.get("method").and_then(serde_json::Value::as_str)
                == Some("textDocument/publishDiagnostics")
            {
                let params = message.get("params").ok_or_else(|| {
                    WorkspaceExecutionError::Protocol(
                        "publishDiagnostics notification omitted params".into(),
                    )
                })?;
                if params.get("uri") == Some(&serde_json::json!(document_uri.clone())) {
                    let raw = params
                        .get("diagnostics")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| {
                            WorkspaceExecutionError::Protocol(
                                "publishDiagnostics did not contain a diagnostics array".into(),
                            )
                        })?;
                    let parsed = raw
                        .iter()
                        .take(DIAGNOSTIC_LIMIT)
                        .map(parse_diagnostic)
                        .collect::<Vec<_>>();
                    if !parsed.is_empty() || !supports_pull_diagnostics {
                        if parsed.is_empty() {
                            empty_push_report = Some(parsed);
                            empty_push_settle_deadline =
                                Some(tokio::time::Instant::now() + Duration::from_secs(5));
                            continue;
                        }
                        break parsed;
                    }
                    empty_push_report = Some(parsed);
                }
            } else if message.get("id") == Some(&serde_json::json!(2))
                && message.get("method").is_none()
            {
                if let Some(error) = message.get("error") {
                    let message_text = error
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if !message_text.is_empty() && !message_text.contains("Method not found") {
                        return Err(WorkspaceExecutionError::Protocol(format!(
                            "language server diagnostics request failed: {}",
                            message_text.chars().take(512).collect::<String>()
                        )));
                    }
                    if let Some(pushed) = empty_push_report.take() {
                        break pushed;
                    }
                } else if let Some(items) = message
                    .pointer("/result/items")
                    .and_then(serde_json::Value::as_array)
                {
                    let parsed = items
                        .iter()
                        .take(DIAGNOSTIC_LIMIT)
                        .map(parse_diagnostic)
                        .collect::<Vec<_>>();
                    if !parsed.is_empty() {
                        break parsed;
                    }
                    empty_push_report = Some(parsed);
                    empty_push_settle_deadline = Some(
                        tokio::time::Instant::now() + Duration::from_secs(5),
                    );
                } else {
                    return Err(WorkspaceExecutionError::Protocol(
                        "language server returned an invalid pull-diagnostics report".into(),
                    ));
                }
            } else if message.get("method").is_some() && message.get("id").is_some() {
                answer_lsp_request(&mut attached.input, &message).await?;
            }
        };
        let _ = send_lsp_message(
            &mut attached.input,
            &serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":document_uri}}}),
        )
        .await;
        let _ = send_lsp_message(
            &mut attached.input,
            &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}),
        )
        .await;
        let _ = send_lsp_message(
            &mut attached.input,
            &serde_json::json!({"jsonrpc":"2.0","method":"exit","params":null}),
        )
        .await;
        Ok(diagnostics)
    }
}

#[async_trait]
impl WorkspaceJobExecutor for BollardWorkspaceJobExecutor {
    async fn run(
        &self,
        request: WorkspaceRunRequest,
    ) -> Result<WorkspaceRunResult, WorkspaceExecutionError> {
        validate_run_request(&request)?;
        let started = Instant::now();
        let name = self
            .create_job_container(
                &request.image_reference,
                &request.checkout_path,
                &request.job_name,
                &request.argv,
                request.timeout_ms,
                false,
            )
            .await?;
        let result = async {
            self.docker
                .start_container(&name, None::<StartContainerOptions<String>>)
                .await
                .map_err(|error| WorkspaceExecutionError::Runtime(error.to_string()))?;
            let (exit_code, timed_out) = self.wait_for_exit(&name, request.timeout_ms).await?;
            let (output, output_truncated) = self.read_logs(&name, OUTPUT_LIMIT_BYTES).await?;
            Ok(WorkspaceRunResult {
                exit_code,
                timed_out,
                output_truncated,
                output,
                elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            })
        }
        .await;
        self.remove_job_container(&name).await;
        result
    }

    async fn diagnostics(
        &self,
        request: WorkspaceDiagnosticsRequest,
    ) -> Result<WorkspaceDiagnosticsResult, WorkspaceExecutionError> {
        self.diagnostics_session(request).await
    }
}

fn validate_checkout(path: &Path) -> Result<PathBuf, WorkspaceExecutionError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(WorkspaceExecutionError::Invalid(
            "workspace job mount must be a real directory".into(),
        ));
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
    validate_snapshot_contents(&canonical)?;
    Ok(canonical)
}

fn validate_snapshot_contents(root: &Path) -> Result<(), WorkspaceExecutionError> {
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut total_bytes = 0u64;
    let mut file_count = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 64 {
            return Err(WorkspaceExecutionError::Invalid(
                "workspace snapshot exceeds the directory-depth limit".into(),
            ));
        }
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if is_secret_component(&name) {
                return Err(WorkspaceExecutionError::Invalid(
                    "workspace snapshot contains a secret path".into(),
                ));
            }
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
            if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
                return Err(WorkspaceExecutionError::Invalid(
                    "workspace snapshot contains a symbolic link or reparse point".into(),
                ));
            }
            if metadata.is_dir() {
                pending.push((path, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if has_multiple_links(&path, &metadata) {
                return Err(WorkspaceExecutionError::Invalid(
                    "workspace snapshot contains a multiply-linked file".into(),
                ));
            }
            file_count = file_count.saturating_add(1);
            total_bytes = total_bytes.saturating_add(metadata.len());
            if metadata.len() > 10 * 1024 * 1024
                || total_bytes > 100 * 1024 * 1024
                || file_count > 10_000
            {
                return Err(WorkspaceExecutionError::Invalid(
                    "workspace snapshot exceeds its file or size budget".into(),
                ));
            }
        }
    }
    Ok(())
}

fn is_secret_component(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    (lower.starts_with('.')
        && [".env", ".ssh", ".git-credentials", ".npmrc", ".pypirc"].contains(&lower.as_str()))
        || [
            "credentials",
            "secrets",
            "id_rsa",
            "id_ed25519",
            ".pem",
            ".key",
            ".p12",
            ".pfx",
            ".keystore",
            ".kubeconfig",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

#[cfg(unix)]
fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(unix)]
fn has_multiple_links(_path: &Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() > 1
}

#[cfg(windows)]
fn has_multiple_links(path: &Path, _metadata: &std::fs::Metadata) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return true;
    };
    winapi_util::file::information(file)
        .map(|information| information.number_of_links() != 1)
        .unwrap_or(true)
}

fn container_user(path: &Path) -> Result<String, WorkspaceExecutionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path)
            .map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
        return Ok(format!("{}:{}", metadata.uid(), metadata.gid()));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok("1000:1000".into())
    }
}

fn docker_host_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(dos) = path.strip_prefix(r"\\?\") {
            return dos.to_owned();
        }
    }
    path.into_owned()
}

fn validate_job_name(name: &str) -> Result<(), WorkspaceExecutionError> {
    if name.is_empty()
        || name.len() > 96
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(WorkspaceExecutionError::Invalid(
            "workspace job name is invalid".into(),
        ));
    }
    Ok(())
}

fn validate_argv(argv: &[String]) -> Result<(), WorkspaceExecutionError> {
    if argv.is_empty()
        || argv.len() > 64
        || argv
            .iter()
            .any(|argument| argument.is_empty() || argument.contains('\0'))
        || argv.iter().map(String::len).sum::<usize>() > 16 * 1024
    {
        return Err(WorkspaceExecutionError::Invalid(
            "workspace command must contain 1 to 64 bounded argv items".into(),
        ));
    }
    Ok(())
}

fn validate_timeout(timeout_ms: u64) -> Result<(), WorkspaceExecutionError> {
    if !(1_000..=180_000).contains(&timeout_ms) {
        return Err(WorkspaceExecutionError::Invalid(
            "workspace job timeout must be between 1 and 180 seconds".into(),
        ));
    }
    Ok(())
}

fn validate_run_request(request: &WorkspaceRunRequest) -> Result<(), WorkspaceExecutionError> {
    validate_job_name(&request.job_name)?;
    validate_argv(&request.argv)?;
    validate_timeout(request.timeout_ms)?;
    Ok(())
}

fn validate_lsp_request(
    request: &WorkspaceDiagnosticsRequest,
) -> Result<(), WorkspaceExecutionError> {
    validate_job_name(&request.job_name)?;
    validate_argv(&request.server_argv)?;
    validate_timeout(request.timeout_ms)?;
    if request.language_id.is_empty()
        || request.language_id.len() > 64
        || request.path.is_empty()
        || request.path.len() > 240
        || request.path.starts_with('/')
        || request.path.contains('\\')
        || request
            .path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || request.text.len() > 1024 * 1024
    {
        return Err(WorkspaceExecutionError::Invalid(
            "workspace diagnostics path, language, or text exceeds its bounds".into(),
        ));
    }
    let root = std::fs::canonicalize(&request.checkout_path)
        .map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
    let mut current = root.clone();
    for component in Path::new(&request.path).components() {
        let std::path::Component::Normal(component) = component else {
            return Err(WorkspaceExecutionError::Invalid(
                "workspace diagnostics path must be relative".into(),
            ));
        };
        let component = component.to_string_lossy();
        if is_secret_component(&component) {
            return Err(WorkspaceExecutionError::Invalid(
                "workspace diagnostics path is secret".into(),
            ));
        }
        current.push(component.as_ref());
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
        if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            return Err(WorkspaceExecutionError::Invalid(
                "workspace diagnostics path crosses a link".into(),
            ));
        }
    }
    let metadata = std::fs::metadata(&current)
        .map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
    if !metadata.is_file()
        || has_multiple_links(&current, &metadata)
        || metadata.len() > 1024 * 1024
    {
        return Err(WorkspaceExecutionError::Invalid(
            "workspace diagnostics file is not a bounded, singly-linked regular file".into(),
        ));
    }
    let actual_text = std::fs::read_to_string(&current)
        .map_err(|error| WorkspaceExecutionError::Invalid(error.to_string()))?;
    if actual_text != request.text {
        return Err(WorkspaceExecutionError::Invalid(
            "workspace diagnostics text differs from the mounted file".into(),
        ));
    }
    Ok(())
}

fn container_name(job_name: &str) -> String {
    format!("execlaw-workspace-{}", job_name.to_ascii_lowercase())
}

fn parse_diagnostic(raw: &serde_json::Value) -> WorkspaceDiagnostic {
    WorkspaceDiagnostic {
        range: raw.get("range").cloned().unwrap_or(serde_json::Value::Null),
        severity: raw
            .get("severity")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u8::try_from(value).ok()),
        code: raw.get("code").cloned(),
        source: raw
            .get("source")
            .and_then(serde_json::Value::as_str)
            .map(|value| value.chars().take(128).collect()),
        message: raw
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map(|value| value.chars().take(4096).collect())
            .unwrap_or_default(),
    }
}

fn bounded_json(value: &serde_json::Value, limit: usize) -> String {
    value.to_string().chars().take(limit).collect()
}

async fn send_lsp_message<W>(
    input: &mut W,
    value: &serde_json::Value,
) -> Result<(), WorkspaceExecutionError>
where
    W: tokio::io::AsyncWrite + Send + Unpin + ?Sized,
{
    let body = serde_json::to_vec(value)
        .map_err(|error| WorkspaceExecutionError::Protocol(error.to_string()))?;
    if body.len() > LSP_FRAME_LIMIT_BYTES {
        return Err(WorkspaceExecutionError::Protocol(
            "outgoing LSP message exceeds 1 MiB".into(),
        ));
    }
    input
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .map_err(|error| WorkspaceExecutionError::Protocol(error.to_string()))?;
    input
        .write_all(&body)
        .await
        .map_err(|error| WorkspaceExecutionError::Protocol(error.to_string()))?;
    input
        .flush()
        .await
        .map_err(|error| WorkspaceExecutionError::Protocol(error.to_string()))
}

#[derive(Default)]
struct LspFrameDecoder {
    bytes: Vec<u8>,
}

impl LspFrameDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<(), WorkspaceExecutionError> {
        if self.bytes.len().saturating_add(chunk.len()) > LSP_FRAME_LIMIT_BYTES + 8 * 1024 {
            return Err(WorkspaceExecutionError::Protocol(
                "LSP stream frame exceeds its buffer bound".into(),
            ));
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    fn pop(&mut self) -> Result<Option<serde_json::Value>, WorkspaceExecutionError> {
        let Some(header_end) = self
            .bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        else {
            if self.bytes.len() > 8 * 1024 {
                return Err(WorkspaceExecutionError::Protocol(
                    "LSP header exceeds 8 KiB".into(),
                ));
            }
            return Ok(None);
        };
        let header = std::str::from_utf8(&self.bytes[..header_end])
            .map_err(|_| WorkspaceExecutionError::Protocol("LSP header is not UTF-8".into()))?;
        let content_length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .ok_or_else(|| {
                WorkspaceExecutionError::Protocol("LSP header omitted Content-Length".into())
            })?;
        if content_length > LSP_FRAME_LIMIT_BYTES {
            return Err(WorkspaceExecutionError::Protocol(
                "incoming LSP message exceeds 1 MiB".into(),
            ));
        }
        let body_start = header_end + 4;
        let body_end = body_start + content_length;
        if self.bytes.len() < body_end {
            return Ok(None);
        }
        let body = self.bytes[body_start..body_end].to_vec();
        self.bytes.drain(..body_end);
        let value = serde_json::from_slice(&body)
            .map_err(|error| WorkspaceExecutionError::Protocol(error.to_string()))?;
        Ok(Some(value))
    }
}

async fn next_lsp_message<S>(
    output: &mut S,
    decoder: &mut LspFrameDecoder,
    deadline: tokio::time::Instant,
    stderr: &mut Vec<u8>,
) -> Result<serde_json::Value, WorkspaceExecutionError>
where
    S: futures_util::Stream<Item = Result<LogOutput, bollard::errors::Error>>
        + Send
        + Unpin
        + ?Sized,
{
    loop {
        if let Some(message) = decoder.pop()? {
            return Ok(message);
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(WorkspaceExecutionError::Protocol(
                "language server did not publish diagnostics before the deadline".into(),
            ));
        }
        let item = tokio::time::timeout(remaining, output.next())
            .await
            .map_err(|_| {
                WorkspaceExecutionError::Protocol(
                    "language server did not publish diagnostics before the deadline".into(),
                )
            })?
            .ok_or_else(|| {
                WorkspaceExecutionError::Protocol(
                    "language server closed stdout before publishing diagnostics".into(),
                )
            })?
            .map_err(|error| WorkspaceExecutionError::Runtime(error.to_string()))?;
        match item {
            LogOutput::StdOut { message } | LogOutput::Console { message } => {
                decoder.push(&message)?;
            }
            LogOutput::StdErr { message } => {
                let remaining = (8 * 1024usize).saturating_sub(stderr.len());
                stderr.extend_from_slice(&message[..message.len().min(remaining)]);
            }
            LogOutput::StdIn { .. } => {}
        }
    }
}

async fn answer_lsp_request<W>(
    input: &mut W,
    request: &serde_json::Value,
) -> Result<(), WorkspaceExecutionError>
where
    W: tokio::io::AsyncWrite + Send + Unpin + ?Sized,
{
    let Some(id) = request.get("id") else {
        return Ok(());
    };
    let method = request
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "workspace/configuration" => {
            let count = request
                .pointer("/params/items")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len);
            serde_json::Value::Array(vec![serde_json::Value::Null; count.min(64)])
        }
        "workspace/workspaceFolders" => serde_json::json!([{
            "uri":"file:///workspace","name":"workspace"
        }]),
        "workspace/applyEdit" => {
            serde_json::json!({"applied":false,"failureReason":"workspace edits require an explicit patch tool call"})
        }
        _ => serde_json::Value::Null,
    };
    send_lsp_message(
        input,
        &serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_and_job_path_bounds_reject_shell_control_and_symlink_mounts() {
        assert!(validate_argv(&["cargo".into(), "test".into()]).is_ok());
        assert!(validate_argv(&["cargo\0 test".into()]).is_err());
        assert!(validate_argv(&[]).is_err());
        assert!(validate_job_name("run-123").is_ok());
        assert!(validate_job_name("../../etc").is_err());
        assert!(validate_timeout(1_000).is_ok());
        assert!(validate_timeout(180_001).is_err());
    }

    #[test]
    fn lsp_frame_decoder_handles_partial_and_multiple_frames() {
        let mut decoder = LspFrameDecoder::default();
        let first = serde_json::json!({"jsonrpc":"2.0","id":1});
        let second = serde_json::json!({"jsonrpc":"2.0","method":"initialized"});
        let a = serde_json::to_vec(&first).unwrap();
        let b = serde_json::to_vec(&second).unwrap();
        let mut bytes = format!("Content-Length: {}\r\n\r\n", a.len()).into_bytes();
        bytes.extend_from_slice(&a);
        bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", b.len()).as_bytes());
        bytes.extend_from_slice(&b);
        let midpoint = bytes.len() / 2;
        decoder.push(&bytes[..midpoint]).unwrap();
        assert!(decoder.pop().unwrap().is_none());
        decoder.push(&bytes[midpoint..]).unwrap();
        assert_eq!(decoder.pop().unwrap().unwrap(), first);
        assert_eq!(decoder.pop().unwrap().unwrap(), second);
    }
}
