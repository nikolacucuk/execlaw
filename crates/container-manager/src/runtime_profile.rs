//! Shared least-privilege profiles for isolated tool runtimes.

use bollard::secret::{HostConfig, ResourcesUlimits};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProfile {
    Runner,
    Browser,
    Parser,
    Coding,
    IntegrationSidecar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeExecution {
    Container,
    NativeProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeProfileLimits {
    pub pids: i64,
    pub memory_bytes: i64,
    pub nano_cpus: i64,
    pub nofile_soft: i64,
    pub nofile_hard: i64,
    pub tmpfs_bytes: u64,
    pub networkless: bool,
}

impl RuntimeProfile {
    pub fn limits(self) -> RuntimeProfileLimits {
        match self {
            Self::Runner => RuntimeProfileLimits {
                pids: 256,
                memory_bytes: 2 * 1024 * 1024 * 1024,
                nano_cpus: 2_000_000_000,
                nofile_soft: 1024,
                nofile_hard: 2048,
                tmpfs_bytes: 64 * 1024 * 1024,
                networkless: false,
            },
            Self::Browser => RuntimeProfileLimits {
                pids: 128,
                memory_bytes: 2 * 1024 * 1024 * 1024,
                nano_cpus: 2_000_000_000,
                nofile_soft: 1024,
                nofile_hard: 2048,
                tmpfs_bytes: 128 * 1024 * 1024,
                networkless: false,
            },
            Self::Parser => RuntimeProfileLimits {
                pids: 64,
                memory_bytes: 1024 * 1024 * 1024,
                nano_cpus: 1_000_000_000,
                nofile_soft: 512,
                nofile_hard: 1024,
                tmpfs_bytes: 64 * 1024 * 1024,
                networkless: true,
            },
            Self::Coding => RuntimeProfileLimits {
                pids: 128,
                memory_bytes: 2 * 1024 * 1024 * 1024,
                nano_cpus: 2_000_000_000,
                nofile_soft: 1024,
                nofile_hard: 2048,
                tmpfs_bytes: 64 * 1024 * 1024,
                networkless: true,
            },
            Self::IntegrationSidecar => RuntimeProfileLimits {
                pids: 128,
                memory_bytes: 1024 * 1024 * 1024,
                nano_cpus: 1_000_000_000,
                nofile_soft: 1024,
                nofile_hard: 2048,
                tmpfs_bytes: 64 * 1024 * 1024,
                networkless: false,
            },
        }
    }

    /// Agent runtime profiles never fall back to an unsandboxed host process.
    pub fn supports(self, execution: RuntimeExecution) -> bool {
        matches!(execution, RuntimeExecution::Container)
    }

    /// These profiles use Linux container controls (seccomp/no-new-privileges,
    /// cgroup PID/CPU/memory limits and Linux mount semantics). A daemon that
    /// reports another OS must fail closed instead of emulating the profile.
    pub fn supports_docker_ostype(self, os_type: &str) -> bool {
        os_type.eq_ignore_ascii_case("linux")
    }

    /// Apply the common profile contract to a Docker host configuration.
    pub fn apply_to_host_config(self, config: &mut HostConfig) {
        let limits = self.limits();
        if limits.networkless {
            config.network_mode = Some("none".into());
        }
        config.readonly_rootfs = Some(true);
        config.cap_drop = Some(vec!["ALL".into()]);
        config.security_opt = Some(vec!["no-new-privileges:true".into()]);
        config.pids_limit = Some(config.pids_limit.map_or(limits.pids, |current| current.min(limits.pids)));
        config.memory = Some(config.memory.map_or(limits.memory_bytes, |current| current.min(limits.memory_bytes)));
        config.nano_cpus = Some(config.nano_cpus.map_or(limits.nano_cpus, |current| current.min(limits.nano_cpus)));
        config.ulimits = Some(vec![ResourcesUlimits {
            name: Some("nofile".into()),
            soft: Some(limits.nofile_soft),
            hard: Some(limits.nofile_hard),
        }]);
        let mut tmpfs = config.tmpfs.take().unwrap_or_default();
        tmpfs.insert(
            "/tmp".into(),
            format!("rw,noexec,nosuid,nodev,size={},mode=1777", limits.tmpfs_bytes),
        );
        config.tmpfs = Some(tmpfs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_budgets_are_distinct_and_native_fallback_is_denied() {
        let profiles = [
            RuntimeProfile::Runner,
            RuntimeProfile::Browser,
            RuntimeProfile::Parser,
            RuntimeProfile::Coding,
            RuntimeProfile::IntegrationSidecar,
        ];
        for profile in profiles {
            assert!(profile.supports(RuntimeExecution::Container));
            assert!(!profile.supports(RuntimeExecution::NativeProcess));
            assert!(profile.supports_docker_ostype("linux"));
            assert!(!profile.supports_docker_ostype("windows"));
            assert!(!profile.supports_docker_ostype(""));
            let limits = profile.limits();
            assert!(limits.pids > 0);
            assert!(limits.memory_bytes > 0);
            assert!(limits.nano_cpus > 0);
            assert!(limits.nofile_soft > 0 && limits.nofile_hard >= limits.nofile_soft);
            assert!(limits.tmpfs_bytes > 0);
        }
        assert!(RuntimeProfile::Parser.limits().networkless);
        assert!(RuntimeProfile::Coding.limits().networkless);
        assert!(!RuntimeProfile::Browser.limits().networkless);
        assert_ne!(
            RuntimeProfile::Parser.limits().memory_bytes,
            RuntimeProfile::Browser.limits().memory_bytes
        );
    }

    #[test]
    fn profile_applies_no_privilege_and_aggregate_resource_controls() {
        let mut config = HostConfig::default();
        config.network_mode = Some("operator-network".into());
        RuntimeProfile::Coding.apply_to_host_config(&mut config);
        assert_eq!(config.network_mode.as_deref(), Some("none"));
        assert_eq!(config.readonly_rootfs, Some(true));
        assert_eq!(config.cap_drop.as_deref(), Some(["ALL".to_owned()].as_slice()));
        assert_eq!(config.security_opt.as_deref(), Some(["no-new-privileges:true".to_owned()].as_slice()));
        assert_eq!(config.pids_limit, Some(RuntimeProfile::Coding.limits().pids));
        assert_eq!(config.memory, Some(RuntimeProfile::Coding.limits().memory_bytes));
        assert_eq!(config.ulimits.as_ref().unwrap()[0].name.as_deref(), Some("nofile"));
        assert!(config.tmpfs.as_ref().unwrap()["/tmp"].contains("noexec"));
    }
}
