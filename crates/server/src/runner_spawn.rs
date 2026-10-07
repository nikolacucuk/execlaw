//! Bollard-driven runner container + workspace volume management.
//!
//! Sibling to `runner_supervisor` — the supervisor owns the in-
//! memory registry and WS plumbing; this module owns the Docker
//! side of the lifecycle:
//!
//!   * `spawn(spec)` — creates the workspace volume if needed,
//!     calls `containers.create()` with the runner image,
//!     starts the container, returns the container_id. The
//!     supervisor's pending-spawn entry must already exist;
//!     `EXECLAW_SPAWN_SECRET` is read from `spec.secret`.
//!
//!   * `kill(container_id)` — `docker stop -t 5` then
//!     `containers.remove()`. Idempotent.
//!
//!   * `wipe_volume(group_id)` — `docker volume rm
//!     execlaw-runner-<group_id>`. Captures pre-removal size for
//!     telemetry. Idempotent (404 = already gone, treat as ok).
//!
//!   * `boot_orphan_sweep(known_group_ids)` — list every
//!     `execlaw-runner-*` volume; remove the ones whose group
//!     is no longer in `state_principal_groups`.
//!
//! Implemented behind a small trait (`RunnerLauncher`) so tests
//! can swap in a `MockRunnerLauncher` and drive the supervisor's
//! lifecycle without Docker. Same pattern as
//! `container_manager::ServiceController`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[cfg(any(test, feature = "test-mock"))]
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;
#[cfg(any(test, feature = "test-mock"))]
use tokio::sync::Mutex;

/// Inputs to `RunnerLauncher::spawn`. Constructed by the
/// supervisor's `ensure(group_id)` path after it's minted the
/// pending-spawn secret + resolved the inference URL.
#[derive(Debug, Clone)]
pub struct RunnerSpec {
    pub group_id: String,
    /// `execlaw/runner:<tag>`. Operator-overridable; defaults to
    /// `execlaw/runner:dev` in dev builds.
    pub image: String,
    /// Hex-encoded one-time spawn secret. Forwarded as
    /// `EXECLAW_SPAWN_SECRET` env var.
    pub spawn_secret_hex: String,
    /// `ws://host.docker.internal:3031` style. Runner appends
    /// `/api/runner/register/<group_id>`.
    pub rpc_url: String,
    /// Default vLLM URL the runner should hit. Per-turn overrides
    /// land via `TurnRequest.inference_url`; this is the fallback.
    pub inference_url: String,
    /// Optional memory cap in bytes. `None` = no cap.
    pub memory_bytes: Option<i64>,
    /// Docker network the container should attach to. The
    /// supervisor passes the same network the inference backend
    /// container lives on so the runner can reach vLLM.
    pub network: Option<String>,
    /// Extra env vars (e.g. `RUST_LOG`).
    pub env: Vec<(String, String)>,
}

/// Result of a spawn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerHandleId {
    pub container_id: String,
    pub volume_name: String,
}

#[derive(Debug, Error)]
pub enum LauncherError {
    #[error("docker error: {0}")]
    Docker(String),
    #[error("runner container resource admission refused: {0}")]
    ResourceAdmission(String),
    #[error("operation timed out")]
    Timeout,
}

/// Abstract spawning of runner containers. Production uses
/// `BollardRunnerLauncher`; tests use `MockRunnerLauncher` to
/// exercise the supervisor's lifecycle without Docker.
#[async_trait]
pub trait RunnerLauncher: Send + Sync {
    async fn spawn(&self, spec: &RunnerSpec) -> Result<RunnerHandleId, LauncherError>;
    async fn kill(&self, container_id: &str) -> Result<(), LauncherError>;
    /// Returns Ok(bytes_freed) on a successful remove (None when
    /// the daemon doesn't tell us). Ok(0) when the volume was
    /// already gone.
    async fn wipe_volume(&self, group_id: &str) -> Result<Option<u64>, LauncherError>;
    /// Returns the volume names currently tracked by the daemon
    /// that match the `execlaw-runner-` prefix.
    async fn list_runner_volumes(&self) -> Result<Vec<String>, LauncherError>;
    /// Namespace prefix for volumes this launcher owns. Mock launchers keep
    /// the historical unscoped names; production scopes names per database.
    fn volume_prefix(&self) -> String {
        "execlaw-runner-".to_owned()
    }
    /// Physical workspace volume name for a principal group.
    fn volume_name_for(&self, group_id: &str) -> String {
        format!("{}{group_id}", self.volume_prefix())
    }
    /// True when the daemon already has a local image matching
    /// `image` (e.g. `execlaw/runner:dev`). Used at boot so the
    /// supervisor disables itself rather than failing every spawn
    /// when the operator hasn't built the image yet. Default impl
    /// returns true for Mock launchers (which always claim the
    /// image is there); the bollard impl actually checks.
    async fn image_present(&self, image: &str) -> bool {
        let _ = image;
        true
    }
}

/// Volume name used by the launcher. Single source of truth.
pub fn volume_name_for(group_id: &str) -> String {
    format!("execlaw-runner-{group_id}")
}

/// Real bollard-backed launcher. Production wiring.
pub struct BollardRunnerLauncher {
    docker: bollard::Docker,
    provenance: Option<execlaw_core::artifact_provenance::ArtifactProvenanceStore>,
    spawn_gate: AsyncMutex<()>,
    installation_id: String,
}

const DEFAULT_RUNNER_MEMORY_BYTES: i64 = 1024 * 1024 * 1024;

fn runner_reservation_ceiling_bytes(
    daemon_total_bytes: Option<i64>,
    host_available_bytes: i64,
    existing_reserved_bytes: i64,
) -> i64 {
    daemon_total_bytes
        .filter(|total| *total > 0)
        .map(|total| total / 2)
        .unwrap_or_else(|| host_available_bytes.saturating_add(existing_reserved_bytes) / 2)
}

impl BollardRunnerLauncher {
    pub fn new() -> Result<Self, LauncherError> {
        let docker = bollard::Docker::connect_with_local_defaults()
            .map_err(|e| LauncherError::Docker(format!("connect: {e}")))?;
        Ok(Self {
            docker,
            provenance: None,
            spawn_gate: AsyncMutex::new(()),
            installation_id: uuid::Uuid::new_v4().simple().to_string(),
        })
    }

    pub fn new_with_provenance(db: execlaw_core::Database) -> Result<Self, LauncherError> {
        let mut launcher = Self::new()?;
        launcher.installation_id = load_or_create_runner_installation_id(&db)?;
        launcher.provenance =
            Some(execlaw_core::artifact_provenance::ArtifactProvenanceStore::new(db));
        Ok(launcher)
    }

    pub fn with_docker(docker: bollard::Docker) -> Self {
        Self {
            docker,
            provenance: None,
            spawn_gate: AsyncMutex::new(()),
            installation_id: uuid::Uuid::new_v4().simple().to_string(),
        }
    }
}

fn load_or_create_runner_installation_id(
    db: &execlaw_core::Database,
) -> Result<String, LauncherError> {
    let proposed = uuid::Uuid::new_v4().simple().to_string();
    db.with_conn(|connection| {
        connection.execute(
            "INSERT OR IGNORE INTO config_runtime_settings(key,value) VALUES ('runner_installation_id',?1)",
            [&proposed],
        )?;
        connection
            .query_row(
                "SELECT value FROM config_runtime_settings WHERE key='runner_installation_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(execlaw_core::db::DbError::from)
    })
    .map_err(|error| LauncherError::Docker(format!("runner installation id: {error}")))
}

#[async_trait]
impl RunnerLauncher for BollardRunnerLauncher {
    async fn spawn(&self, spec: &RunnerSpec) -> Result<RunnerHandleId, LauncherError> {
        use bollard::container::{
            Config, CreateContainerOptions, ListContainersOptions, StartContainerOptions,
        };
        use bollard::secret::HostConfig;

        let _spawn_guard = self.spawn_gate.lock().await;

        self.provenance
            .as_ref()
            .ok_or_else(|| {
                LauncherError::Docker(
                    "artifact provenance store is not configured for runner launch".into(),
                )
            })?
            .authorize_oci_reference(
                "runner:image",
                execlaw_core::artifact_provenance::ArtifactType::RunnerImage,
                &spec.image,
                "runner-launcher",
            )
            .map_err(|error| LauncherError::Docker(error.to_string()))?;

        let image = self
            .docker
            .inspect_image(&spec.image)
            .await
            .map_err(|error| LauncherError::Docker(format!("inspect runner isolation profile: {error}")))?;
        if !execlaw_container_manager::RuntimeProfile::Runner
            .image_user_is_non_root(image.config.as_ref().and_then(|config| config.user.as_deref()))
        {
            return Err(LauncherError::ResourceAdmission(
                "runner runtime profile requires an image with a non-root USER".into(),
            ));
        }

        let volume = self.volume_name_for(&spec.group_id);
        let container_name = volume.clone();
        let memory_bytes = spec.memory_bytes.unwrap_or(DEFAULT_RUNNER_MEMORY_BYTES);
        if memory_bytes <= 0 {
            return Err(LauncherError::ResourceAdmission(
                "runner memory limit must be positive".into(),
            ));
        }
        if memory_bytes > execlaw_container_manager::RuntimeProfile::Runner.limits().memory_bytes {
            return Err(LauncherError::ResourceAdmission(
                "runner memory request exceeds the Runner runtime profile".into(),
            ));
        }
        let _ = self
            .docker
            .remove_container(
                &container_name,
                Some(bollard::container::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        let available_bytes = execlaw_container_manager::hardware::available_ram_mb()
            .and_then(|available| available.checked_mul(1024 * 1024))
            .and_then(|available| i64::try_from(available).ok())
            .ok_or_else(|| {
                LauncherError::ResourceAdmission("live host RAM sample is unavailable".into())
            })?;
        if available_bytes < memory_bytes {
            return Err(LauncherError::ResourceAdmission(format!(
                "requested {memory_bytes} bytes with {available_bytes} available",
            )));
        }
        let active = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: false,
                ..Default::default()
            }))
            .await
            .map_err(|error| LauncherError::Docker(format!("list active runners: {error}")))?;
        let reserved_bytes = active
            .iter()
            .filter_map(|container| {
                let is_runner = container.labels.as_ref().is_some_and(|labels| {
                    labels
                        .get("execlaw.kind")
                        .is_some_and(|kind| kind == "runner")
                }) || container.names.as_ref().is_some_and(|names| {
                    names
                        .iter()
                        .any(|name| name.starts_with("/execlaw-runner-"))
                });
                is_runner.then(|| {
                    container
                        .labels
                        .as_ref()
                        .map(|labels| {
                            labels
                                .get("execlaw.memory_bytes")
                                .and_then(|value| value.parse::<i64>().ok())
                                .unwrap_or(DEFAULT_RUNNER_MEMORY_BYTES)
                        })
                        .unwrap_or(DEFAULT_RUNNER_MEMORY_BYTES)
                })
            })
            .fold(0_i64, i64::saturating_add);
        let daemon_total_bytes = self
            .docker
            .info()
            .await
            .ok()
            .and_then(|info| info.mem_total);
        let memory_budget_bytes =
            runner_reservation_ceiling_bytes(daemon_total_bytes, available_bytes, reserved_bytes);
        if memory_budget_bytes <= 0
            || reserved_bytes.saturating_add(memory_bytes) > memory_budget_bytes
        {
            return Err(LauncherError::ResourceAdmission(format!(
                "runner reservations would reach {} bytes, above the {} byte ceiling",
                reserved_bytes.saturating_add(memory_bytes),
                memory_budget_bytes
            )));
        }

        // Create the workspace volume (idempotent — ignore "already exists").
        let _ = self
            .docker
            .create_volume(bollard::volume::CreateVolumeOptions::<String> {
                name: volume.clone(),
                driver: "local".into(),
                driver_opts: Default::default(),
                labels: [
                    ("execlaw.group_id".into(), spec.group_id.clone()),
                    ("execlaw.kind".into(), "runner-workspace".into()),
                    (
                        "execlaw.installation_id".into(),
                        self.installation_id.clone(),
                    ),
                ]
                .into_iter()
                .collect(),
            })
            .await;

        // Build env list for the container.
        let mut env = vec![
            format!("EXECLAW_RPC_URL={}", spec.rpc_url),
            format!("EXECLAW_GROUP_ID={}", spec.group_id),
            format!("EXECLAW_SPAWN_SECRET={}", spec.spawn_secret_hex),
            format!("EXECLAW_INFERENCE_URL={}", spec.inference_url),
        ];
        for (k, v) in &spec.env {
            env.push(format!("{k}={v}"));
        }

        // host-gateway alias: on Linux Docker, `host.docker.internal`
        // is NOT auto-defined; on Docker Desktop (Windows / macOS)
        // it is. Adding `host.docker.internal:host-gateway` is
        // harmless on Desktop (the daemon already maps the name)
        // and load-bearing on Linux (so the runner can reach
        // vLLM / our WS endpoint on the host's loopback).
        let mut host_cfg = HostConfig {
            binds: Some(vec![format!("{}:/workspace", volume)]),
            memory: Some(memory_bytes),
            network_mode: spec.network.clone(),
            extra_hosts: Some(vec!["host.docker.internal:host-gateway".to_owned()]),
            ..Default::default()
        };
        execlaw_container_manager::RuntimeProfile::Runner.apply_to_host_config(&mut host_cfg);

        let labels = HashMap::from([
            ("execlaw.kind".to_owned(), "runner".to_owned()),
            ("execlaw.group_id".to_owned(), spec.group_id.clone()),
            ("execlaw.memory_bytes".to_owned(), memory_bytes.to_string()),
            (
                "execlaw.installation_id".to_owned(),
                self.installation_id.clone(),
            ),
        ]);
        let cfg = Config {
            image: Some(spec.image.clone()),
            env: Some(env),
            labels: Some(labels),
            host_config: Some(host_cfg),
            ..Default::default()
        };

        // Best-effort remove any stale container with the same
        // name (e.g. from a server crash mid-spawn) before trying
        // create. A name conflict 409 surfaces as Docker; we pre-
        // empt it.
        let create = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: container_name.clone(),
                    platform: None,
                }),
                cfg,
            )
            .await
            .map_err(|e| LauncherError::Docker(format!("create: {e}")))?;

        self.docker
            .start_container(&create.id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| LauncherError::Docker(format!("start: {e}")))?;

        Ok(RunnerHandleId {
            container_id: create.id,
            volume_name: volume,
        })
    }

    async fn kill(&self, container_id: &str) -> Result<(), LauncherError> {
        let _ = self
            .docker
            .stop_container(
                container_id,
                Some(bollard::container::StopContainerOptions { t: 5 }),
            )
            .await;
        let _ = self
            .docker
            .remove_container(
                container_id,
                Some(bollard::container::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        Ok(())
    }

    async fn wipe_volume(&self, group_id: &str) -> Result<Option<u64>, LauncherError> {
        let name = self.volume_name_for(group_id);
        // Ignore 404 — the volume might already be gone.
        let _ = self.docker.remove_volume(&name, None).await;
        Ok(None)
    }

    async fn list_runner_volumes(&self) -> Result<Vec<String>, LauncherError> {
        let result = self
            .docker
            .list_volumes(None::<bollard::volume::ListVolumesOptions<String>>)
            .await
            .map_err(|e| LauncherError::Docker(format!("list_volumes: {e}")))?;
        let names = result
            .volumes
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| {
                if v.name.starts_with(&self.volume_prefix())
                    && v.labels.get("execlaw.installation_id") == Some(&self.installation_id)
                {
                    Some(v.name)
                } else {
                    None
                }
            })
            .collect();
        Ok(names)
    }

    fn volume_prefix(&self) -> String {
        // Legacy installations sweep every `execlaw-runner-*` volume. Use a
        // disjoint prefix so an older control plane cannot erase this one's
        // workspace when both installations share a Docker daemon.
        format!("execlaw-install-{}-runner-", self.installation_id)
    }

    async fn image_present(&self, image: &str) -> bool {
        // bollard's `inspect_image` returns Err(404) when the image
        // isn't local. Other errors (especially Docker-socket
        // permission denials in a containerized control plane) need
        // logging; callers still receive false so boot remains safe.
        match self.docker.inspect_image(image).await {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(%image, %error, "runner image inspection failed");
                false
            }
        }
    }
}

/// Test/dev mock. Records calls in a vec so tests can assert what
/// the supervisor asked for. Spawn always succeeds + emits a fake
/// container id; kill / wipe are no-ops bookkeeping-wise.
#[cfg(any(test, feature = "test-mock"))]
pub struct MockRunnerLauncher {
    inner: Arc<Mutex<MockState>>,
}

#[cfg(any(test, feature = "test-mock"))]
#[derive(Default)]
struct MockState {
    spawned: Vec<RunnerSpec>,
    killed: Vec<String>,
    wiped: Vec<String>,
    /// Volume names the mock claims exist (used by
    /// `list_runner_volumes`). Tests can pre-populate this to
    /// exercise the orphan-sweep path.
    known_volumes: Vec<String>,
    next_container_seq: u32,
}

#[cfg(any(test, feature = "test-mock"))]
impl Default for MockRunnerLauncher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-mock"))]
impl MockRunnerLauncher {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockState::default())),
        }
    }

    pub async fn spawn_count(&self) -> usize {
        self.inner.lock().await.spawned.len()
    }

    pub async fn killed(&self) -> Vec<String> {
        self.inner.lock().await.killed.clone()
    }

    pub async fn wiped(&self) -> Vec<String> {
        self.inner.lock().await.wiped.clone()
    }

    pub async fn seed_volume(&self, name: impl Into<String>) {
        self.inner.lock().await.known_volumes.push(name.into());
    }
}

#[cfg(any(test, feature = "test-mock"))]
#[async_trait]
impl RunnerLauncher for MockRunnerLauncher {
    async fn spawn(&self, spec: &RunnerSpec) -> Result<RunnerHandleId, LauncherError> {
        let mut s = self.inner.lock().await;
        s.spawned.push(spec.clone());
        s.next_container_seq += 1;
        let cid = format!("mock-cid-{}", s.next_container_seq);
        let vol = volume_name_for(&spec.group_id);
        s.known_volumes.push(vol.clone());
        Ok(RunnerHandleId {
            container_id: cid,
            volume_name: vol,
        })
    }

    async fn kill(&self, container_id: &str) -> Result<(), LauncherError> {
        self.inner.lock().await.killed.push(container_id.to_owned());
        Ok(())
    }

    async fn wipe_volume(&self, group_id: &str) -> Result<Option<u64>, LauncherError> {
        let mut s = self.inner.lock().await;
        let name = volume_name_for(group_id);
        s.wiped.push(name.clone());
        s.known_volumes.retain(|v| v != &name);
        Ok(None)
    }

    async fn list_runner_volumes(&self) -> Result<Vec<String>, LauncherError> {
        Ok(self.inner.lock().await.known_volumes.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(group_id: &str) -> RunnerSpec {
        RunnerSpec {
            group_id: group_id.to_owned(),
            image: "execlaw/runner:dev".into(),
            spawn_secret_hex: "ff".repeat(32),
            rpc_url: "ws://test:3031".into(),
            inference_url: "http://infer:8000/v1".into(),
            memory_bytes: Some(2 * 1024 * 1024 * 1024),
            network: None,
            env: vec![],
        }
    }

    #[test]
    fn volume_name_uses_prefix() {
        assert_eq!(volume_name_for("abc"), "execlaw-runner-abc");
    }

    #[test]
    fn runner_ceiling_uses_daemon_total_without_double_counting_live_runners() {
        let gib = 1024_i64 * 1024 * 1024;
        let ceiling = runner_reservation_ceiling_bytes(Some(8 * gib), 4 * gib, gib);
        assert_eq!(ceiling, 4 * gib);
        assert!(gib + 2 * gib <= ceiling);
        assert!(3 * gib + 2 * gib > ceiling);
        assert_eq!(
            runner_reservation_ceiling_bytes(None, 4 * gib, gib),
            5 * gib / 2
        );
    }

    #[test]
    fn runner_installation_id_is_stable_per_database_and_distinct_between_databases() {
        use execlaw_core::db::{Database, DbConfig};
        use execlaw_core::migrations::MigrationRunner;

        let first = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&first).apply_all().unwrap();
        let second = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&second).apply_all().unwrap();

        let first_id = load_or_create_runner_installation_id(&first).unwrap();
        assert_eq!(
            load_or_create_runner_installation_id(&first).unwrap(),
            first_id
        );
        let second_id = load_or_create_runner_installation_id(&second).unwrap();
        assert_ne!(first_id, second_id);
        assert_ne!(
            format!("execlaw-install-{first_id}-runner-controller"),
            format!("execlaw-install-{second_id}-runner-controller")
        );
    }

    #[tokio::test]
    async fn mock_spawn_records_spec_and_creates_volume() {
        let m = MockRunnerLauncher::new();
        let id = m.spawn(&spec("g-1")).await.unwrap();
        assert!(id.container_id.starts_with("mock-cid-"));
        assert_eq!(id.volume_name, "execlaw-runner-g-1");
        assert_eq!(m.spawn_count().await, 1);
        let vols = m.list_runner_volumes().await.unwrap();
        assert!(vols.contains(&"execlaw-runner-g-1".to_owned()));
    }

    #[tokio::test]
    async fn mock_kill_records_container_id() {
        let m = MockRunnerLauncher::new();
        m.kill("cid-1").await.unwrap();
        assert_eq!(m.killed().await, vec!["cid-1"]);
    }

    #[tokio::test]
    async fn mock_wipe_volume_removes_from_known_list() {
        let m = MockRunnerLauncher::new();
        let _ = m.spawn(&spec("g-1")).await.unwrap();
        m.wipe_volume("g-1").await.unwrap();
        assert_eq!(m.wiped().await, vec!["execlaw-runner-g-1"]);
        let vols = m.list_runner_volumes().await.unwrap();
        assert!(!vols.contains(&"execlaw-runner-g-1".to_owned()));
    }

    #[tokio::test]
    async fn mock_list_volumes_includes_seeded() {
        let m = MockRunnerLauncher::new();
        m.seed_volume("execlaw-runner-orphan-1").await;
        m.seed_volume("execlaw-runner-orphan-2").await;
        let vols = m.list_runner_volumes().await.unwrap();
        assert_eq!(vols.len(), 2);
    }
}
