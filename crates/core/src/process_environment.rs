//! Minimal inherited environment for trusted host-launched helper processes.

use std::ffi::{OsStr, OsString};

const INHERITED_KEYS: &[&str] = &[
    "PATH",
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "TEMP",
    "TMP",
    "HOME",
    "USERPROFILE",
    "USERNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
    "DOCKER_TLS_VERIFY",
    "DOCKER_CERT_PATH",
];

/// Filter the parent process environment before launching a helper.
///
/// Callers may add purpose-specific values after clearing the environment.
/// Credentials are never inherited by name or wildcard; Docker connection
/// settings are retained explicitly because the host uses them to reach its
/// local container service.
pub fn minimal_inherited_environment(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    variables
        .into_iter()
        .filter(|(name, _)| {
            INHERITED_KEYS
                .iter()
                .any(|allowed| name.eq_ignore_ascii_case(OsStr::new(allowed)))
        })
        .collect()
}

/// Read only the explicitly retained OS environment for a child process.
pub fn current_minimal_environment() -> Vec<(OsString, OsString)> {
    minimal_inherited_environment(std::env::vars_os())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_os_and_local_container_settings_are_inherited() {
        let values = vec![
            (OsString::from("PATH"), OsString::from("/bin")),
            (
                OsString::from("DOCKER_CONTEXT"),
                OsString::from("desktop-linux"),
            ),
            (OsString::from("OPENAI_API_KEY"), OsString::from("secret")),
            (
                OsString::from("AWS_SECRET_ACCESS_KEY"),
                OsString::from("secret"),
            ),
            (
                OsString::from("EXECLAW_MASTER_KEY"),
                OsString::from("secret"),
            ),
        ];
        let result = minimal_inherited_environment(values);
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|(name, _)| name == OsStr::new("PATH")));
        assert!(
            result
                .iter()
                .any(|(name, _)| name == OsStr::new("DOCKER_CONTEXT"))
        );
    }
}
