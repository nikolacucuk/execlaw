use execlaw_container_manager::{
    BollardWorkspaceJobExecutor, WorkspaceDiagnosticsRequest, WorkspaceJobExecutor,
    WorkspaceRunRequest,
};
use execlaw_core::{Database, DbConfig, MigrationRunner};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let image_reference = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run -p execlaw-container-manager --example workspace_toolchain_smoke -- <local-sha256-id>")?;
    let database = Database::open(&DbConfig::in_memory_unencrypted())?;
    MigrationRunner::new(&database).apply_all()?;
    let provenance =
        execlaw_core::artifact_provenance::ArtifactProvenanceStore::new(database.clone());
    let mut policy = provenance.policy()?;
    policy.allow_unsigned_local_development = true;
    provenance.configure("Controller", "workspace-toolchain-smoke", &policy)?;

    let workspace = tempfile::tempdir()?;
    std::fs::create_dir_all(workspace.path().join("src"))?;
    std::fs::write(
        workspace.path().join("Cargo.toml"),
        "[package]\nname = \"workspace-job-smoke\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    std::fs::write(
        workspace.path().join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"workspace-job-smoke\"\nversion = \"0.1.0\"\n",
    )?;
    std::fs::write(
        workspace.path().join("src/lib.rs"),
        "pub fn multiply(a: i32, b: i32) -> i32 { a * b }\n\n#[cfg(test)]\nmod tests { #[test] fn multiplies() { assert_eq!(super::multiply(6, 7), 42); } }\n",
    )?;
    let broken_source = "pub fn broken( -> i32 {\n";
    std::fs::write(workspace.path().join("src/broken.rs"), broken_source)?;

    let executor = BollardWorkspaceJobExecutor::connect(database)?;
    let run = executor
        .run(WorkspaceRunRequest {
            image_reference: image_reference.clone(),
            checkout_path: workspace.path().to_owned(),
            job_name: "smoke-terminal".into(),
            argv: vec![
                "cargo".into(),
                "test".into(),
                "--offline".into(),
                "--locked".into(),
                "--workspace".into(),
            ],
            cwd: ".".into(),
            stdin: None,
            timeout_ms: 120_000,
        })
        .await?;
    if run.exit_code != Some(0) || !run.output.contains("1 passed") {
        return Err(format!("confined terminal smoke failed: {run:?}").into());
    }
    if workspace.path().join("target").exists() {
        return Err("terminal output escaped its disposable writable /tmp mount".into());
    }
    std::fs::write(
        workspace.path().join("src/lib.rs"),
        "pub mod broken;\npub fn multiply(a: i32, b: i32) -> i32 { a * b }\n\n#[cfg(test)]\nmod tests { #[test] fn multiplies() { assert_eq!(super::multiply(6, 7), 42); } }\n",
    )?;

    let diagnostics = executor
        .diagnostics(WorkspaceDiagnosticsRequest {
            image_reference,
            checkout_path: workspace.path().to_owned(),
            job_name: "smoke-rust-analyzer".into(),
            server_argv: vec!["rust-analyzer".into()],
            path: "src/broken.rs".into(),
            language_id: "rust".into(),
            text: broken_source.into(),
            timeout_ms: 120_000,
        })
        .await?;
    if !diagnostics
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Some(1))
    {
        return Err(
            format!("language server did not return an error diagnostic: {diagnostics:?}").into(),
        );
    }
    println!(
        "workspace_toolchain_smoke_ok terminal_exit=0 diagnostics={}",
        diagnostics.diagnostics.len()
    );
    Ok(())
}
