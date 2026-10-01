//! Reproducible real-task scoring. Expected answers and verifiers are loaded
//! from the suite directory, never copied into the model's prompt/workspace.

use anyhow::{Context, bail};
use execlaw_container_manager::{
    BollardWorkspaceJobExecutor, WorkspaceDiagnosticsRequest, WorkspaceJobExecutor,
    WorkspaceRunRequest,
};
use execlaw_inference_api::{ChatMessage, ChatRequest, InferenceClient, ModelId, ToolDeclaration};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct Suite {
    name: String,
    version: String,
    tasks: Vec<Task>,
}

#[derive(Debug, Deserialize)]
struct Task {
    id: String,
    category: String,
    prompt: String,
    #[serde(flatten)]
    verifier: Verifier,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "verifier", rename_all = "snake_case")]
enum Verifier {
    Coding {
        fixture_workspace: PathBuf,
        output_file: PathBuf,
        fixture_response: String,
    },
    Research {
        sources: Vec<Source>,
        required_terms: Vec<String>,
        fixture_response: String,
    },
    Memory {
        required_terms: Vec<String>,
        fixture_response: String,
    },
    Automation {
        expected_effects: Vec<Value>,
        fixture_response: String,
    },
    WorkspaceCoding {
        fixture_workspace: PathBuf,
        expected_workspace_files: BTreeMap<String, String>,
        test_argv: Vec<String>,
        language_servers: BTreeMap<String, Vec<String>>,
        required_tools: Vec<String>,
        fixture_response: String,
    },
}

#[derive(Debug, Deserialize)]
struct Source {
    id: String,
    text: String,
    #[serde(default = "default_source_fetched")]
    fetched_ok: bool,
}

#[derive(Debug, Deserialize)]
struct WorkspaceFixtureAction {
    tool: String,
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct WorkspaceFileEdit {
    path: String,
    expected_sha256: Option<String>,
    content: String,
}

fn default_source_fetched() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    schema_version: u32,
    started_at_utc: String,
    suite: String,
    suite_version: String,
    dataset_sha256: String,
    model: String,
    backend: String,
    quantization: String,
    hardware_tier: String,
    host_os: String,
    host_arch: String,
    arm: String,
    seed_start: u64,
    runs: u32,
    max_tokens: u32,
    offline_fixture: bool,
    workspace_toolchain_fingerprint_sha256: Option<String>,
    results: Vec<TaskResult>,
    summary: Summary,
    comparison: Option<Comparison>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TaskResult {
    trial: u32,
    seed: u64,
    task_id: String,
    category: String,
    success: bool,
    failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Summary {
    attempted: usize,
    succeeded: usize,
    failed: usize,
    success_rate: f64,
    wilson_95_low: f64,
    wilson_95_high: f64,
    by_category: BTreeMap<String, CategorySummary>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CategorySummary {
    attempted: usize,
    succeeded: usize,
    success_rate: f64,
    wilson_95_low: f64,
    wilson_95_high: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Comparison {
    baseline_arm: String,
    candidate_arm: String,
    baseline_success_rate: f64,
    candidate_success_rate: f64,
    paired_success_delta: f64,
    paired_delta_95_low: f64,
    paired_delta_95_high: f64,
    paired_attempts: usize,
}

pub(super) struct RunConfig {
    pub(super) runs: u32,
    pub(super) seed: u64,
    pub(super) arm: String,
    pub(super) backend: String,
    pub(super) quantization: String,
    pub(super) hardware_tier: String,
    pub(super) max_tokens: u32,
    pub(super) offline_fixture: bool,
    pub(super) allow_executing_generated_code: bool,
    pub(super) compare_path: Option<PathBuf>,
    pub(super) base_url: Option<String>,
    pub(super) model: String,
    pub(super) workspace_image: Option<String>,
    pub(super) approve_workspace_image: bool,
}

pub(super) async fn run(
    suite_path: PathBuf,
    output_path: PathBuf,
    config: RunConfig,
) -> anyhow::Result<()> {
    let RunConfig {
        runs,
        seed,
        arm,
        backend,
        quantization,
        hardware_tier,
        max_tokens,
        offline_fixture,
        allow_executing_generated_code,
        compare_path,
        base_url,
        model,
        workspace_image,
        approve_workspace_image,
    } = config;
    if runs == 0 || max_tokens == 0 {
        bail!("--runs and --max-tokens must be greater than zero");
    }
    let suite_bytes = std::fs::read(&suite_path)
        .with_context(|| format!("read benchmark suite {}", suite_path.display()))?;
    let suite: Suite =
        serde_json::from_slice(&suite_bytes).context("parse benchmark suite JSON")?;
    if suite.tasks.is_empty() {
        bail!("benchmark suite has no tasks");
    }
    validate_suite(&suite)?;
    if suite.tasks.iter().any(|task| {
        matches!(
            &task.verifier,
            Verifier::Coding { .. } | Verifier::WorkspaceCoding { .. }
        )
    }) && !offline_fixture
        && !allow_executing_generated_code
    {
        bail!(
            "coding tasks execute model-produced code; pass --allow-executing-generated-code to acknowledge"
        );
    }
    let workspace_executor: Option<Arc<dyn WorkspaceJobExecutor>> = if suite
        .tasks
        .iter()
        .any(|task| matches!(&task.verifier, Verifier::WorkspaceCoding { .. }))
    {
        let image = workspace_image
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("workspace coding tasks require --workspace-image"))?;
        if !approve_workspace_image {
            bail!(
                "workspace coding benchmark requires --approve-workspace-image to authorize this exact local digest"
            );
        }
        let db = execlaw_core::Database::open(&execlaw_core::DbConfig::in_memory_unencrypted())?;
        execlaw_core::MigrationRunner::new(&db).apply_all()?;
        execlaw_core::artifact_provenance::ArtifactProvenanceStore::new(db.clone())
            .approve_controller_oci_reference(
                execlaw_core::artifact_provenance::ArtifactType::Sidecar,
                image,
                "Controller",
                "eval-harness-workspace-benchmark",
            )?;
        Some(Arc::new(BollardWorkspaceJobExecutor::connect(db)?))
    } else {
        None
    };
    let comparison_baseline = if let Some(path) = &compare_path {
        let baseline: Record = serde_json::from_slice(
            &std::fs::read(path)
                .with_context(|| format!("read baseline record {}", path.display()))?,
        )
        .context("parse baseline benchmark record")?;
        let dataset_sha256 = hex::encode(Sha256::digest(&suite_bytes));
        if baseline.dataset_sha256 != dataset_sha256
            || baseline.model != model
            || baseline.backend != backend
            || baseline.quantization != quantization
            || baseline.hardware_tier != hardware_tier
            || baseline.runs != runs
            || baseline.seed_start != seed
            || baseline.max_tokens != max_tokens
            || baseline.offline_fixture != offline_fixture
            || baseline.workspace_toolchain_fingerprint_sha256
                != workspace_image
                    .as_deref()
                    .map(|image| hex::encode(Sha256::digest(image.as_bytes())))
        {
            bail!("baseline comparison metadata does not match this run");
        }
        Some(baseline)
    } else {
        None
    };

    let endpoint = base_url
        .or_else(|| std::env::var("EXECLAW_INFERENCE_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8000/v1".to_owned());
    let client = InferenceClient::new(endpoint);
    let mut results = Vec::with_capacity(suite.tasks.len() * runs as usize);
    let started_at_utc = chrono::Utc::now().to_rfc3339();

    for trial in 0..runs {
        let task_seed = seed.wrapping_add(u64::from(trial));
        for task in &suite.tasks {
            if let Verifier::WorkspaceCoding { .. } = &task.verifier {
                let result = run_workspace_coding_task(
                    &client,
                    &model,
                    task,
                    task_seed,
                    max_tokens,
                    offline_fixture,
                    workspace_executor
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("workspace executor is not configured"))?,
                    workspace_image
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("workspace image is not configured"))?,
                )
                .await;
                let (success, evidence, failure) = match result {
                    Ok(evidence) => (true, Some(evidence), None),
                    Err(error) => (false, None, Some(format!("{error:#}"))),
                };
                results.push(TaskResult {
                    trial,
                    seed: task_seed,
                    task_id: task.id.clone(),
                    category: task.category.clone(),
                    success,
                    failure,
                    evidence,
                });
                continue;
            }
            let completion = if offline_fixture {
                Ok(task.fixture_response().to_owned())
            } else {
                request_task(&client, &model, task, task_seed, max_tokens).await
            };
            let result = match completion {
                Ok(output) => verify_task(task, &output).await,
                Err(error) => Err(format!("inference failure: {error:#}")),
            };
            results.push(TaskResult {
                trial,
                seed: task_seed,
                task_id: task.id.clone(),
                category: task.category.clone(),
                success: result.is_ok(),
                failure: result.err(),
                evidence: None,
            });
        }
    }

    let summary = summarize(&results);
    let mut record = Record {
        schema_version: 1,
        started_at_utc,
        suite: suite.name,
        suite_version: suite.version,
        dataset_sha256: hex::encode(Sha256::digest(&suite_bytes)),
        model,
        backend,
        quantization,
        hardware_tier,
        host_os: std::env::consts::OS.to_owned(),
        host_arch: std::env::consts::ARCH.to_owned(),
        arm,
        seed_start: seed,
        runs,
        max_tokens,
        offline_fixture,
        workspace_toolchain_fingerprint_sha256: workspace_image
            .as_deref()
            .map(|image| hex::encode(Sha256::digest(image.as_bytes()))),
        results,
        summary,
        comparison: None,
    };
    if let Some(baseline) = comparison_baseline {
        record.comparison = Some(compare_records(&baseline, &record)?);
    }
    if let Some(parent) = output_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create result directory {}", parent.display()))?;
    }
    let serialized = serde_json::to_vec_pretty(&record)?;
    std::fs::write(&output_path, serialized)
        .with_context(|| format!("write benchmark record {}", output_path.display()))?;
    println!("benchmark record: {}", output_path.display());
    println!(
        "tasks: {}/{} succeeded; success {:.1}% (95% Wilson {:.1}%–{:.1}%)",
        record.summary.succeeded,
        record.summary.attempted,
        record.summary.success_rate * 100.0,
        record.summary.wilson_95_low * 100.0,
        record.summary.wilson_95_high * 100.0,
    );
    if let Some(comparison) = &record.comparison {
        println!(
            "paired improvement over {}: {:+.1}% (95% CI {:+.1}% to {:+.1}%, n={})",
            comparison.baseline_arm,
            comparison.paired_success_delta * 100.0,
            comparison.paired_delta_95_low * 100.0,
            comparison.paired_delta_95_high * 100.0,
            comparison.paired_attempts,
        );
    }
    if record.summary.failed > 0 {
        bail!(
            "benchmark had {} failed task attempts",
            record.summary.failed
        );
    }
    Ok(())
}

fn compare_records(baseline: &Record, candidate: &Record) -> anyhow::Result<Comparison> {
    if baseline.dataset_sha256 != candidate.dataset_sha256
        || baseline.model != candidate.model
        || baseline.backend != candidate.backend
        || baseline.quantization != candidate.quantization
        || baseline.hardware_tier != candidate.hardware_tier
        || baseline.runs != candidate.runs
        || baseline.seed_start != candidate.seed_start
        || baseline.max_tokens != candidate.max_tokens
        || baseline.offline_fixture != candidate.offline_fixture
        || baseline.workspace_toolchain_fingerprint_sha256
            != candidate.workspace_toolchain_fingerprint_sha256
    {
        bail!(
            "baseline comparison requires matching dataset, model, backend, quantization, hardware tier, seeds, runs, token budget, and execution mode"
        );
    }
    let baseline_by_key: BTreeMap<_, _> = baseline
        .results
        .iter()
        .map(|result| {
            (
                (result.trial, result.seed, result.task_id.as_str()),
                result.success,
            )
        })
        .collect();
    let mut differences = Vec::with_capacity(candidate.results.len());
    for result in &candidate.results {
        let key = (result.trial, result.seed, result.task_id.as_str());
        let prior = baseline_by_key.get(&key).ok_or_else(|| {
            anyhow::anyhow!(
                "baseline is missing task {} at trial {}",
                result.task_id,
                result.trial
            )
        })?;
        differences
            .push((if result.success { 1.0 } else { 0.0 }) - (if *prior { 1.0 } else { 0.0 }));
    }
    if differences.is_empty() || differences.len() != baseline.results.len() {
        bail!("baseline and candidate must contain the same non-empty task attempts");
    }
    let delta = differences.iter().sum::<f64>() / differences.len() as f64;
    let variance = if differences.len() > 1 {
        differences
            .iter()
            .map(|value| (value - delta).powi(2))
            .sum::<f64>()
            / (differences.len() - 1) as f64
    } else {
        0.0
    };
    let margin = 1.959_963_984_540_054 * (variance / differences.len() as f64).sqrt();
    let candidate_successes = candidate
        .results
        .iter()
        .filter(|result| result.success)
        .count();
    Ok(Comparison {
        baseline_arm: baseline.arm.clone(),
        candidate_arm: candidate.arm.clone(),
        baseline_success_rate: baseline.summary.success_rate,
        candidate_success_rate: candidate_successes as f64 / candidate.results.len() as f64,
        paired_success_delta: delta,
        paired_delta_95_low: (delta - margin).max(-1.0),
        paired_delta_95_high: (delta + margin).min(1.0),
        paired_attempts: differences.len(),
    })
}

impl Task {
    fn fixture_response(&self) -> &str {
        match &self.verifier {
            Verifier::Coding {
                fixture_response, ..
            }
            | Verifier::Research {
                fixture_response, ..
            }
            | Verifier::Memory {
                fixture_response, ..
            }
            | Verifier::Automation {
                fixture_response, ..
            }
            | Verifier::WorkspaceCoding {
                fixture_response, ..
            } => fixture_response,
        }
    }
}

fn validate_suite(suite: &Suite) -> anyhow::Result<()> {
    let mut ids = std::collections::HashSet::new();
    for task in &suite.tasks {
        if task.id.trim().is_empty() || !ids.insert(task.id.as_str()) {
            bail!("benchmark task ids must be non-empty and unique");
        }
        if task.prompt.trim().is_empty() {
            bail!("task {} has an empty prompt", task.id);
        }
        if let Verifier::Coding {
            fixture_workspace,
            output_file,
            ..
        } = &task.verifier
        {
            if !output_file.is_relative()
                || output_file
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                bail!("task {} output_file must be a safe relative path", task.id);
            }
            if !fixture_workspace.join("Cargo.toml").is_file() {
                bail!("task {} coding fixture must contain Cargo.toml", task.id);
            }
        }
        if let Verifier::WorkspaceCoding {
            fixture_workspace,
            expected_workspace_files,
            test_argv,
            language_servers,
            required_tools,
            fixture_response,
        } = &task.verifier
        {
            if !fixture_workspace.is_relative()
                || fixture_workspace
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
                || !fixture_workspace.join("Cargo.toml").is_file()
            {
                bail!(
                    "task {} workspace fixture must be a safe Cargo project path",
                    task.id
                );
            }
            if expected_workspace_files.is_empty()
                || expected_workspace_files
                    .keys()
                    .any(|path| !valid_workspace_relative_path(path))
                || expected_workspace_files
                    .values()
                    .any(|content| content.len() > 1024 * 1024)
            {
                bail!(
                    "task {} expected workspace files are empty, unsafe, or oversized",
                    task.id
                );
            }
            if test_argv.is_empty()
                || test_argv.len() > 64
                || test_argv
                    .iter()
                    .any(|arg| arg.is_empty() || arg.contains('\0'))
            {
                bail!(
                    "task {} test_argv must be a bounded non-empty argv vector",
                    task.id
                );
            }
            if language_servers.iter().any(|(language, argv)| {
                language.trim().is_empty()
                    || argv.is_empty()
                    || argv.len() > 32
                    || argv
                        .iter()
                        .any(|arg| arg.is_empty() || arg.len() > 1024 || arg.contains('\0'))
            }) {
                bail!(
                    "task {} contains an invalid language-server argv map",
                    task.id
                );
            }
            if required_tools.is_empty()
                || required_tools.iter().any(|tool| {
                    !matches!(
                        tool.as_str(),
                        "workspace.read_file"
                            | "workspace.search"
                            | "workspace.apply_patch"
                            | "workspace.run"
                            | "workspace.diagnostics"
                    )
                })
                || !required_tools.iter().any(|tool| tool == "workspace.run")
            {
                bail!(
                    "task {} must require the bounded workspace.run test tool",
                    task.id
                );
            }
            let actions: Vec<WorkspaceFixtureAction> = serde_json::from_str(fixture_response)
                .with_context(|| {
                    format!("task {} has invalid workspace fixture actions", task.id)
                })?;
            if actions.is_empty()
                || actions.iter().any(|action| {
                    !required_tools.contains(&action.tool)
                        || !matches!(
                            action.tool.as_str(),
                            "workspace.read_file"
                                | "workspace.search"
                                | "workspace.apply_patch"
                                | "workspace.run"
                                | "workspace.diagnostics"
                        )
                })
                || !actions.iter().any(|action| action.tool == "workspace.run")
            {
                bail!(
                    "task {} fixture actions must include its bounded workspace.run test",
                    task.id
                );
            }
        }
    }
    Ok(())
}

async fn request_task(
    client: &InferenceClient,
    model: &str,
    task: &Task,
    seed: u64,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let mut user_prompt = task.prompt.clone();
    match &task.verifier {
        Verifier::Research { sources, .. } => {
            user_prompt.push_str("\n\nEvidence sources:\n");
            for source in sources {
                if source.fetched_ok {
                    user_prompt.push_str(&format!("[{}] {}\n", source.id, source.text));
                }
            }
            user_prompt.push_str(
                "Cite every factual sentence with one or more exact fetched source IDs as [id]. Do not cite an unavailable source or add a factual claim unsupported by its cited source text.",
            );
        }
        Verifier::Coding { output_file, .. } => user_prompt.push_str(&format!(
            "\nReturn only the complete replacement contents for {}.",
            output_file.display()
        )),
        Verifier::WorkspaceCoding { .. } => user_prompt.push_str(
            "\nUse the workspace read/search/apply_patch/diagnostics/run tools. Apply the repair across all required files, run the supplied offline test command, and finish only after it exits successfully.",
        ),
        Verifier::Automation { .. } => {
            user_prompt.push_str("\nReturn only a JSON array of proposed effects.");
        }
        Verifier::Memory { .. } => {}
    }
    // The OpenAI-compatible request schema used here has no seed field. The
    // seed is still recorded and supplied as neutral run metadata for endpoints
    // that can be configured to derive deterministic sampling externally.
    user_prompt.push_str(&format!("\n[benchmark trial seed: {seed}]"));
    let request = ChatRequest {
        model: ModelId(model.to_owned()),
        messages: vec![
            ChatMessage::system(
                "Complete the task. Do not claim success without the requested artifact.",
            ),
            ChatMessage::user(user_prompt),
        ],
        tools: None,
        stream: false,
        temperature: Some(0.0),
        max_tokens: Some(max_tokens),
        chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
        tool_choice: None,
        response_format: None,
        guided_decoding_backend: None,
    };
    let response = client.chat_completions(&request).await?;
    Ok(response
        .choices
        .first()
        .and_then(|choice| {
            choice
                .message
                .content
                .as_ref()
                .map(|content| content.as_text())
        })
        .unwrap_or_default())
}

const WORKSPACE_EVAL_MAX_ROUNDS: usize = 16;
const WORKSPACE_EVAL_MAX_CALLS: usize = 32;
const WORKSPACE_EVAL_MAX_FILE_BYTES: usize = 1_048_576;
const WORKSPACE_EVAL_MAX_TOTAL_BYTES: usize = 100 * 1024 * 1024;

async fn run_workspace_coding_task(
    client: &InferenceClient,
    model: &str,
    task: &Task,
    seed: u64,
    max_tokens: u32,
    offline_fixture: bool,
    executor: &Arc<dyn WorkspaceJobExecutor>,
    image_reference: &str,
) -> anyhow::Result<Value> {
    let Verifier::WorkspaceCoding {
        fixture_workspace,
        expected_workspace_files,
        test_argv,
        language_servers,
        required_tools,
        fixture_response,
    } = &task.verifier
    else {
        bail!("task is not a workspace coding task");
    };
    let fixture = std::fs::canonicalize(fixture_workspace)
        .with_context(|| format!("open workspace fixture for {}", task.id))?;
    let workspace = copy_workspace_for_container(&fixture)?;

    let mut calls = Vec::new();
    let mut patched = BTreeSet::new();
    let mut diagnostics_count = 0usize;
    let mut successful_test: Option<(String, String)> = None;
    let mut output_tokens = 0u32;
    if offline_fixture {
        let fixture_actions: Vec<WorkspaceFixtureAction> =
            serde_json::from_str(fixture_response).context("parse workspace fixture actions")?;
        for action in fixture_actions {
            if calls.len() >= WORKSPACE_EVAL_MAX_CALLS {
                bail!("workspace benchmark action budget exceeded");
            }
            execute_workspace_tool(
                executor,
                image_reference,
                workspace.path(),
                &task.id,
                &action.tool,
                &action.arguments,
                test_argv,
                language_servers,
                &mut calls,
                &mut patched,
                &mut diagnostics_count,
                &mut successful_test,
            )
            .await?;
        }
    } else {
        let tools = workspace_tool_declarations();
        let mut messages = vec![
            ChatMessage::system(
                "Repair the supplied isolated source workspace. Use only the workspace tools. Inspect files before patching, use SHA-256 preconditions from read_file, request LSP diagnostics where configured, and run the exact offline test command. The snapshot is disposable, has no network, and contains no credentials. Finish only after the test command exits zero. Do not return source contents in prose.",
            ),
            ChatMessage::user(format!(
                "{}\n\nRun this exact offline test argv after your final patch: {:?}\nAvailable diagnostic language IDs: {:?}\nRequired tool calls: {:?}\n[benchmark trial seed: {}]",
                task.prompt,
                test_argv,
                language_servers.keys().collect::<Vec<_>>(),
                required_tools,
                seed
            )),
        ];
        let started = std::time::Instant::now();
        let mut completed = false;
        for _round in 0..WORKSPACE_EVAL_MAX_ROUNDS {
            if started.elapsed() > std::time::Duration::from_secs(600) {
                bail!("workspace benchmark exceeded its 10-minute case budget");
            }
            let remaining_tokens = max_tokens.saturating_sub(output_tokens);
            if remaining_tokens == 0 {
                bail!("workspace benchmark exceeded its cumulative completion-token budget");
            }
            let request = ChatRequest {
                model: ModelId(model.to_owned()),
                messages: messages.clone(),
                tools: Some(tools.clone()),
                stream: false,
                temperature: Some(0.0),
                max_tokens: Some(remaining_tokens),
                chat_template_kwargs: Some(serde_json::json!({"enable_thinking":false})),
                tool_choice: Some(serde_json::json!("auto")),
                response_format: None,
                guided_decoding_backend: None,
            };
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                client.chat_completions(&request),
            )
            .await
            .map_err(|_| anyhow::anyhow!("workspace benchmark inference timeout"))?
            .map_err(|error| {
                anyhow::anyhow!("workspace benchmark inference {}", error.safe_class())
            })?;
            let Some(choice) = response.choices.first() else {
                bail!("workspace benchmark model returned no choice");
            };
            let usage = response
                .usage
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("workspace benchmark model omitted token usage"))?;
            output_tokens = output_tokens.saturating_add(usage.completion_tokens);
            if output_tokens > max_tokens {
                bail!("workspace benchmark exceeded its cumulative token budget");
            }
            if choice.message.tool_calls.is_empty() {
                completed = true;
                break;
            }
            messages.push(choice.message.clone());
            for call in &choice.message.tool_calls {
                if calls.len() >= WORKSPACE_EVAL_MAX_CALLS {
                    bail!("workspace benchmark action budget exceeded");
                }
                let arguments = serde_json::from_str::<Value>(&call.function.arguments)
                    .map_err(|_| anyhow::anyhow!("workspace tool arguments were invalid JSON"))?;
                let tool_result = execute_workspace_tool(
                    executor,
                    image_reference,
                    workspace.path(),
                    &task.id,
                    &call.function.name,
                    &arguments,
                    test_argv,
                    language_servers,
                    &mut calls,
                    &mut patched,
                    &mut diagnostics_count,
                    &mut successful_test,
                )
                .await;
                let response_body = match tool_result {
                    Ok(value) => serde_json::json!({"ok":true,"result":value}),
                    Err(error) => serde_json::json!({"ok":false,"error":error.to_string()}),
                };
                messages.push(ChatMessage::tool_result(
                    call.id.clone(),
                    serde_json::to_string(&response_body)?,
                ));
            }
        }
        if !completed {
            bail!("workspace benchmark exhausted its model-round budget");
        }
    }

    for required in required_tools {
        if !calls.iter().any(|call| call == required) {
            bail!("workspace benchmark omitted required tool {required}");
        }
    }
    let expected_test_hash = workspace_map_sha256(expected_workspace_files)?;
    let Some((observed_test_hash, test_output_sha256)) = successful_test else {
        bail!("workspace benchmark did not produce a successful test exit");
    };
    if observed_test_hash != expected_test_hash {
        bail!("workspace changed after its final successful test run");
    }
    let actual_files = read_workspace_files(workspace.path())?;
    if actual_files != *expected_workspace_files {
        bail!("final workspace files did not match the held-out expected workspace");
    }
    Ok(serde_json::json!({
        "tool_calls":calls,
        "patched_paths":patched,
        "successful_test_output_sha256":test_output_sha256,
        "model_completion_tokens":output_tokens,
        "lsp_diagnostic_count":diagnostics_count,
        "workspace_sha256":expected_test_hash
    }))
}

fn workspace_tool_declarations() -> Vec<ToolDeclaration> {
    vec![
        ToolDeclaration::function(
            "workspace.read_file",
            "Read a bounded UTF-8 file from the temporary benchmark workspace.",
            serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        ),
        ToolDeclaration::function(
            "workspace.search",
            "Search bounded UTF-8 workspace files for a literal query.",
            serde_json::json!({"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":1024}},"required":["query"],"additionalProperties":false}),
        ),
        ToolDeclaration::function(
            "workspace.apply_patch",
            "Apply one or more SHA-256 checked replacements in the temporary workspace.",
            serde_json::json!({"type":"object","properties":{"edits":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","properties":{"path":{"type":"string"},"expected_sha256":{"type":["string","null"]},"content":{"type":"string","maxLength":1048576}},"required":["path","expected_sha256","content"],"additionalProperties":false}}},"required":["edits"],"additionalProperties":false}),
        ),
        ToolDeclaration::function(
            "workspace.run",
            "Run an argv command in a network-disabled, resource-limited, read-only container snapshot.",
            serde_json::json!({"type":"object","properties":{"argv":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"string","minLength":1,"maxLength":4096}},"timeout_ms":{"type":"integer","minimum":1000,"maximum":180000}},"required":["argv"],"additionalProperties":false}),
        ),
        ToolDeclaration::function(
            "workspace.diagnostics",
            "Request LSP diagnostics for a file from the Controller-configured language-server map.",
            serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"language_id":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1000,"maximum":180000}},"required":["path","language_id"],"additionalProperties":false}),
        ),
    ]
}

async fn execute_workspace_tool(
    executor: &Arc<dyn WorkspaceJobExecutor>,
    image_reference: &str,
    workspace: &Path,
    task_id: &str,
    tool_name: &str,
    args: &Value,
    test_argv: &[String],
    language_servers: &BTreeMap<String, Vec<String>>,
    calls: &mut Vec<String>,
    patched: &mut BTreeSet<String>,
    diagnostics_count: &mut usize,
    successful_test: &mut Option<(String, String)>,
) -> anyhow::Result<Value> {
    calls.push(tool_name.to_owned());
    match tool_name {
        "workspace.read_file" => {
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("read_file requires path"))?;
            let (content, sha256) = read_workspace_file(workspace, path)?;
            Ok(serde_json::json!({"path":path,"text":content,"sha256":sha256}))
        }
        "workspace.search" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= 1024)
                .ok_or_else(|| anyhow::anyhow!("search query must be 1..=1024 bytes"))?;
            let mut matches = Vec::new();
            for (path, text) in read_workspace_files(workspace)? {
                for (line, value) in text.lines().enumerate() {
                    if value.contains(query) {
                        matches.push(serde_json::json!({
                            "path":path,"line":line+1,
                            "text":value.chars().take(2048).collect::<String>()
                        }));
                        if matches.len() == 500 {
                            break;
                        }
                    }
                }
                if matches.len() == 500 {
                    break;
                }
            }
            Ok(serde_json::json!({"matches":matches}))
        }
        "workspace.apply_patch" => {
            let edits: Vec<WorkspaceFileEdit> = serde_json::from_value(
                args.get("edits")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("apply_patch requires edits"))?,
            )?;
            if edits.is_empty() || edits.len() > 32 {
                bail!("apply_patch must contain 1..=32 edits");
            }
            let mut results = Vec::with_capacity(edits.len());
            let mut distinct = BTreeSet::new();
            for edit in edits {
                if !valid_workspace_relative_path(&edit.path)
                    || !distinct.insert(edit.path.clone())
                    || edit.content.len() > WORKSPACE_EVAL_MAX_FILE_BYTES
                {
                    bail!("apply_patch path, duplicate, or content budget is invalid");
                }
                let current = read_workspace_file_optional(workspace, &edit.path)?;
                let current_hash = current.as_ref().map(|(_, digest)| digest.as_str());
                let next_hash = hex::encode(Sha256::digest(edit.content.as_bytes()));
                if current_hash == Some(next_hash.as_str()) {
                    results.push(
                        serde_json::json!({"path":edit.path,"sha256":next_hash,"unchanged":true}),
                    );
                    continue;
                }
                if current_hash != edit.expected_sha256.as_deref() {
                    bail!("apply_patch SHA-256 precondition failed for {}", edit.path);
                }
                let target = workspace.join(&edit.path);
                let parent = target
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("patch target has no parent"))?;
                std::fs::create_dir_all(parent)?;
                ensure_workspace_parent(workspace, parent)?;
                std::fs::write(&target, edit.content.as_bytes())?;
                patched.insert(edit.path.clone());
                *successful_test = None;
                results.push(
                    serde_json::json!({"path":edit.path,"sha256":next_hash,"unchanged":false}),
                );
            }
            let total_bytes = read_workspace_files(workspace)?
                .values()
                .map(String::len)
                .sum::<usize>();
            if total_bytes > WORKSPACE_EVAL_MAX_TOTAL_BYTES {
                bail!("workspace exceeds its 100 MiB budget after patch");
            }
            Ok(serde_json::json!({"edits":results}))
        }
        "workspace.run" => {
            let argv = parse_workspace_argv(args)?;
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(60_000);
            if !(1_000..=180_000).contains(&timeout_ms) {
                bail!("workspace job timeout must be 1000..=180000 ms");
            }
            let snapshot = copy_workspace_for_container(workspace)?;
            let job_name = workspace_job_name(task_id, calls.len());
            let result = executor
                .run(WorkspaceRunRequest {
                    image_reference: image_reference.to_owned(),
                    checkout_path: snapshot.path().to_owned(),
                    job_name,
                    argv: argv.clone(),
                    timeout_ms,
                })
                .await?;
            if argv == test_argv && result.exit_code == Some(0) && !result.timed_out {
                *successful_test = Some((
                    workspace_map_sha256(&read_workspace_files(workspace)?)?,
                    hex::encode(Sha256::digest(result.output.as_bytes())),
                ));
            }
            Ok(serde_json::json!({
                "exit_code":result.exit_code,"timed_out":result.timed_out,
                "output_truncated":result.output_truncated,"output":result.output,
                "elapsed_ms":result.elapsed_ms,"was_configured_test":argv==test_argv
            }))
        }
        "workspace.diagnostics" => {
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("diagnostics requires path"))?;
            let language_id = args
                .get("language_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("diagnostics requires language_id"))?;
            let server_argv = language_servers.get(language_id).ok_or_else(|| {
                anyhow::anyhow!("no configured language server for {language_id}")
            })?;
            let (text, _) = read_workspace_file(workspace, path)?;
            let snapshot = copy_workspace_for_container(workspace)?;
            let result = executor
                .diagnostics(WorkspaceDiagnosticsRequest {
                    image_reference: image_reference.to_owned(),
                    checkout_path: snapshot.path().to_owned(),
                    job_name: workspace_job_name(task_id, calls.len()),
                    server_argv: server_argv.clone(),
                    path: path.to_owned(),
                    language_id: language_id.to_owned(),
                    text,
                    timeout_ms: args
                        .get("timeout_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(30_000),
                })
                .await?;
            *diagnostics_count += result.diagnostics.len();
            Ok(
                serde_json::json!({"language_id":result.language_id,"path":result.path,"diagnostics":result.diagnostics,"elapsed_ms":result.elapsed_ms}),
            )
        }
        other => bail!("workspace tool is not in the benchmark tool catalog: {other}"),
    }
}

fn parse_workspace_argv(args: &Value) -> anyhow::Result<Vec<String>> {
    let argv = args
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("workspace.run requires argv"))?
        .iter()
        .map(|argument| {
            argument
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("workspace.run argv entries must be strings"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if argv.is_empty()
        || argv.len() > 64
        || argv
            .iter()
            .any(|value| value.is_empty() || value.contains('\0'))
        || argv.iter().map(String::len).sum::<usize>() > 16 * 1024
    {
        bail!("workspace.run argv is empty or exceeds its bounds");
    }
    Ok(argv)
}

fn valid_workspace_relative_path(path: &str) -> bool {
    let candidate = Path::new(path);
    !path.is_empty()
        && path.len() <= 240
        && !path.contains('\\')
        && !path.contains(':')
        && !candidate.is_absolute()
        && candidate
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && !candidate.components().any(|part| {
            let name = part.as_os_str().to_string_lossy().to_ascii_lowercase();
            name.starts_with('.')
                && [".env", ".ssh", ".npmrc", ".pypirc", ".git-credentials"]
                    .contains(&name.as_str())
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
                .any(|needle| name.contains(needle))
        })
}

fn read_workspace_file(root: &Path, relative: &str) -> anyhow::Result<(String, String)> {
    read_workspace_file_optional(root, relative)?
        .ok_or_else(|| anyhow::anyhow!("workspace file does not exist: {relative}"))
}

fn read_workspace_file_optional(
    root: &Path,
    relative: &str,
) -> anyhow::Result<Option<(String, String)>> {
    if !valid_workspace_relative_path(relative) {
        bail!("workspace path is absolute, traversing, secret, or invalid");
    }
    let canonical_root = std::fs::canonicalize(root)?;
    let mut current = canonical_root.clone();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            bail!("workspace path contains a non-normal component");
        };
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            bail!("workspace path crosses a symbolic link");
        }
    }
    let canonical = std::fs::canonicalize(&current)?;
    if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
        bail!("workspace file resolves outside the temporary workspace");
    }
    let metadata = std::fs::metadata(&canonical)?;
    if metadata.len() as usize > WORKSPACE_EVAL_MAX_FILE_BYTES {
        bail!("workspace file exceeds the 1 MiB read limit");
    }
    let bytes = std::fs::read(&canonical)?;
    let text = String::from_utf8(bytes.clone())
        .map_err(|_| anyhow::anyhow!("workspace file is not UTF-8"))?;
    Ok(Some((text, hex::encode(Sha256::digest(bytes)))))
}

fn ensure_workspace_parent(root: &Path, parent: &Path) -> anyhow::Result<()> {
    let root = std::fs::canonicalize(root)?;
    let parent = std::fs::canonicalize(parent)?;
    if !parent.starts_with(&root) {
        bail!("workspace patch parent resolves outside its temporary root");
    }
    let mut current = root.clone();
    for component in parent.strip_prefix(&root)?.components() {
        let Component::Normal(component) = component else {
            bail!("workspace patch parent has a non-normal component");
        };
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("workspace patch parent crosses a link or non-directory");
        }
    }
    Ok(())
}

fn read_workspace_files(root: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    const IGNORED_DIRS: &[&str] = &[
        ".git",
        ".execlaw",
        "node_modules",
        "target",
        ".venv",
        "dist",
    ];
    let canonical_root = std::fs::canonicalize(root)?;
    let mut stack = vec![(canonical_root.clone(), String::new(), 0usize)];
    let mut files = BTreeMap::new();
    let mut total_bytes = 0usize;
    while let Some((directory, relative, depth)) = stack.pop() {
        if depth > 64 {
            bail!("workspace exceeds the 64-level depth limit");
        }
        let mut entries = std::fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || workspace_reparse_point(&metadata) {
                bail!("workspace contains a symbolic link or reparse point");
            }
            if metadata.is_dir() {
                if !IGNORED_DIRS.contains(&name.to_ascii_lowercase().as_str()) {
                    stack.push((path, join_workspace_path(&relative, &name), depth + 1));
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if workspace_multiple_links(&path, &metadata) {
                bail!("workspace contains a multiply-linked file");
            }
            let file_path = join_workspace_path(&relative, &name);
            if !valid_workspace_relative_path(&file_path) {
                bail!("workspace contains a secret path");
            }
            if metadata.len() as usize > WORKSPACE_EVAL_MAX_FILE_BYTES {
                bail!("workspace file exceeds 1 MiB");
            }
            total_bytes = total_bytes.saturating_add(metadata.len() as usize);
            if total_bytes > WORKSPACE_EVAL_MAX_TOTAL_BYTES || files.len() >= 10_000 {
                bail!("workspace exceeds its snapshot budget");
            }
            let bytes = std::fs::read(&path)?;
            let text = String::from_utf8(bytes)
                .map_err(|_| anyhow::anyhow!("workspace file is not UTF-8"))?;
            files.insert(file_path, text);
        }
    }
    Ok(files)
}

fn copy_workspace_for_container(source: &Path) -> anyhow::Result<tempfile::TempDir> {
    let files = read_workspace_files(source)?;
    let snapshot = tempfile::tempdir()?;
    let root = std::fs::canonicalize(snapshot.path())?;
    for (relative, content) in files {
        let path = root.join(&relative);
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("workspace file lacks a parent"))?,
        )?;
        ensure_workspace_parent(&root, path.parent().unwrap_or(&root))?;
        std::fs::write(path, content)?;
    }
    Ok(snapshot)
}

fn workspace_map_sha256(files: &BTreeMap<String, String>) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec(files)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn join_workspace_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{parent}/{child}")
    }
}

#[cfg(unix)]
fn workspace_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn workspace_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(unix)]
fn workspace_multiple_links(_path: &Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() > 1
}

#[cfg(windows)]
fn workspace_multiple_links(path: &Path, _metadata: &std::fs::Metadata) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return true;
    };
    winapi_util::file::information(file)
        .map(|information| information.number_of_links() != 1)
        .unwrap_or(true)
}

fn workspace_job_name(task_id: &str, action_count: usize) -> String {
    let digest = hex::encode(Sha256::digest(task_id.as_bytes()));
    format!("eval-{}-{action_count}", &digest[..20])
}

async fn verify_task(task: &Task, output: &str) -> Result<(), String> {
    match &task.verifier {
        Verifier::Coding {
            fixture_workspace,
            output_file,
            ..
        } => {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            copy_workspace(fixture_workspace, temp.path()).map_err(|error| error.to_string())?;
            let target = temp.path().join(output_file);
            std::fs::write(&target, output.trim())
                .map_err(|error| format!("write coding artifact: {error}"))?;
            let mut command = std::process::Command::new("cargo");
            command
                .args(["test", "--offline", "--quiet"])
                .current_dir(temp.path())
                .env("CARGO_NET_OFFLINE", "true");
            for (name, value) in std::env::vars_os() {
                let normalized = name.to_string_lossy().to_ascii_uppercase();
                if ![
                    "KEY",
                    "TOKEN",
                    "SECRET",
                    "PASSWORD",
                    "CREDENTIAL",
                    "AUTH",
                    "API_URL",
                    "DATABASE_URL",
                    "DSN",
                ]
                .iter()
                .any(|marker| normalized.contains(marker))
                {
                    command.env(name, value);
                }
            }
            let result = command
                .output()
                .map_err(|error| format!("start offline workspace tests: {error}"))?;
            if result.status.success() {
                Ok(())
            } else {
                Err("offline workspace tests failed".to_owned())
            }
        }
        Verifier::Research {
            sources,
            required_terms,
            ..
        } => {
            verify_research_claims(output, sources)?;
            check_terms(output, required_terms)
        }
        Verifier::Memory { required_terms, .. } => check_terms(output, required_terms),
        Verifier::Automation {
            expected_effects, ..
        } => {
            let effects: Vec<Value> = serde_json::from_str(output.trim())
                .map_err(|_| "automation response was not a JSON effect array".to_owned())?;
            let mut mock_sink = Vec::new();
            mock_sink.extend(effects);
            if &mock_sink == expected_effects {
                Ok(())
            } else {
                Err("mock sink effects did not match expected effects".to_owned())
            }
        }
        Verifier::WorkspaceCoding { .. } => Err(
            "workspace coding verifier must run through the confined multi-tool workspace loop"
                .into(),
        ),
    }
}

fn verify_research_claims(output: &str, sources: &[Source]) -> Result<(), String> {
    let sources_by_id = sources
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect::<std::collections::HashMap<_, _>>();
    let mut in_references = false;
    let mut claims = 0usize;
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') || line.eq_ignore_ascii_case("sources:") {
            let heading = line
                .trim_start_matches('#')
                .trim()
                .trim_end_matches(':')
                .to_ascii_lowercase();
            in_references = matches!(
                heading.as_str(),
                "sources" | "references" | "evidence review"
            );
            continue;
        }
        if in_references {
            continue;
        }
        let (claim, citations) = strip_research_citations(line);
        if claim.trim().is_empty() {
            continue;
        }
        if citations.is_empty() {
            return Err("research claim omitted a fetched source ID".into());
        }
        claims = claims.saturating_add(1);
        let mut supported = false;
        for id in citations {
            let source = sources_by_id
                .get(id.as_str())
                .ok_or_else(|| format!("research claim cited unknown source ID '{id}'"))?;
            if !source.fetched_ok {
                return Err(format!("research claim cited unavailable source ID '{id}'"));
            }
            supported |=
                execlaw_core::research::research_claim_supported_by_snapshot(&claim, &source.text);
        }
        if !supported {
            return Err("cited source text does not support the research claim".into());
        }
    }
    if claims == 0 {
        return Err("research answer contained no verifiable claims".into());
    }
    Ok(())
}

fn strip_research_citations(line: &str) -> (String, Vec<String>) {
    let mut claim = String::with_capacity(line.len());
    let mut citations = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        claim.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find(']') else {
            claim.push_str(&rest[open..]);
            return (claim, citations);
        };
        let id = &after_open[..close];
        if !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            citations.push(id.to_owned());
        } else {
            claim.push('[');
            claim.push_str(id);
            claim.push(']');
        }
        rest = &after_open[close + 1..];
    }
    claim.push_str(rest);
    (claim, citations)
}

fn check_terms(output: &str, terms: &[String]) -> Result<(), String> {
    let lower = output.to_lowercase();
    let missing: Vec<_> = terms
        .iter()
        .filter(|term| !lower.contains(&term.to_lowercase()))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "answer omitted {} required evidence terms",
            missing.len()
        ))
    }
}

fn copy_workspace(source: &Path, destination: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_workspace(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn summarize(results: &[TaskResult]) -> Summary {
    let succeeded = results.iter().filter(|result| result.success).count();
    let (success_rate, wilson_95_low, wilson_95_high) = wilson(succeeded, results.len());
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for result in results {
        let count = counts.entry(result.category.clone()).or_default();
        count.0 += 1;
        count.1 += usize::from(result.success);
    }
    let by_category = counts
        .into_iter()
        .map(|(category, (attempted, succeeded))| {
            let (success_rate, wilson_95_low, wilson_95_high) = wilson(succeeded, attempted);
            (
                category,
                CategorySummary {
                    attempted,
                    succeeded,
                    success_rate,
                    wilson_95_low,
                    wilson_95_high,
                },
            )
        })
        .collect();
    Summary {
        attempted: results.len(),
        succeeded,
        failed: results.len() - succeeded,
        success_rate,
        wilson_95_low,
        wilson_95_high,
        by_category,
    }
}

fn wilson(successes: usize, attempts: usize) -> (f64, f64, f64) {
    if attempts == 0 {
        return (0.0, 0.0, 0.0);
    }
    let n = attempts as f64;
    let p = successes as f64 / n;
    let z = 1.959_963_984_540_054;
    let denominator = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denominator;
    let margin = z * ((p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt()) / denominator;
    (p, (center - margin).max(0.0), (center + margin).min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockWorkspaceExecutor;

    #[async_trait::async_trait]
    impl WorkspaceJobExecutor for MockWorkspaceExecutor {
        async fn run(
            &self,
            request: WorkspaceRunRequest,
        ) -> Result<
            execlaw_container_manager::WorkspaceRunResult,
            execlaw_container_manager::WorkspaceExecutionError,
        > {
            let current = std::fs::read_to_string(request.checkout_path.join("src/math.rs"))
                .map_err(|error| {
                    execlaw_container_manager::WorkspaceExecutionError::Invalid(error.to_string())
                })?;
            let exit_code = if current.contains("wrapping_add") {
                101
            } else {
                0
            };
            Ok(execlaw_container_manager::WorkspaceRunResult {
                exit_code: Some(exit_code),
                timed_out: false,
                output_truncated: false,
                output: if exit_code == 0 {
                    "2 passed"
                } else {
                    "overflow regression failed"
                }
                .into(),
                elapsed_ms: 5,
            })
        }

        async fn diagnostics(
            &self,
            request: WorkspaceDiagnosticsRequest,
        ) -> Result<
            execlaw_container_manager::WorkspaceDiagnosticsResult,
            execlaw_container_manager::WorkspaceExecutionError,
        > {
            Ok(execlaw_container_manager::WorkspaceDiagnosticsResult {
                language_id: request.language_id,
                path: request.path,
                diagnostics: Vec::new(),
                elapsed_ms: 4,
            })
        }
    }

    #[test]
    fn wilson_interval_contains_observed_rate() {
        let (rate, low, high) = wilson(7, 10);
        assert_eq!(rate, 0.7);
        assert!(low < rate && high > rate);
    }

    #[tokio::test]
    async fn workspace_repair_fixture_requires_exact_multifile_state_and_final_test_pass() {
        let suite: Suite = serde_json::from_str(include_str!(
            "../../../evals/benchmark/workspace-repair-v1.json"
        ))
        .unwrap();
        let task = &suite.tasks[0];
        let Verifier::WorkspaceCoding {
            fixture_workspace,
            expected_workspace_files,
            test_argv,
            language_servers,
            required_tools,
            fixture_response,
        } = &task.verifier
        else {
            panic!("H040 fixture must use workspace_coding verifier");
        };
        let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let fixture_workspace = fixture_root.join(fixture_workspace);
        let workspace = copy_workspace_for_container(&fixture_workspace).unwrap();
        let actions: Vec<WorkspaceFixtureAction> = serde_json::from_str(fixture_response).unwrap();
        let executor: Arc<dyn WorkspaceJobExecutor> = Arc::new(MockWorkspaceExecutor);
        let mut calls = Vec::new();
        let mut patched = BTreeSet::new();
        let mut diagnostics_count = 0;
        let mut successful_test = None;
        for action in actions {
            execute_workspace_tool(
                &executor,
                "sha256:fixture",
                workspace.path(),
                &task.id,
                &action.tool,
                &action.arguments,
                test_argv,
                language_servers,
                &mut calls,
                &mut patched,
                &mut diagnostics_count,
                &mut successful_test,
            )
            .await
            .unwrap();
        }
        for tool in required_tools {
            assert!(
                calls.iter().any(|called| called == tool),
                "missing call to {tool}"
            );
        }
        assert_eq!(
            read_workspace_files(workspace.path()).unwrap(),
            *expected_workspace_files
        );
        let expected_hash = workspace_map_sha256(expected_workspace_files).unwrap();
        assert_eq!(successful_test.unwrap().0, expected_hash);
        assert_eq!(
            patched,
            ["src/lib.rs".to_owned(), "src/math.rs".to_owned()].into()
        );
        assert_eq!(diagnostics_count, 0);
    }

    #[tokio::test]
    async fn automation_verifier_uses_only_the_in_memory_sink() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id":"safe", "category":"automation", "prompt":"emit action",
            "verifier":"automation", "expected_effects":[{"kind":"notify","text":"ok"}],
            "fixture_response":"[]"
        }))
        .unwrap();
        assert!(
            verify_task(&task, r#"[{"kind":"notify","text":"ok"}]"#)
                .await
                .is_ok()
        );
        assert!(
            verify_task(
                &task,
                r#"[{"kind":"http","url":"https://example.invalid"}]"#
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn research_verifier_scores_claim_support_not_citation_shape() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id":"research-proof",
            "category":"research",
            "prompt":"What address and port does the service use?",
            "verifier":"research",
            "sources":[
                {"id":"config","text":"The service listens on loopback address 127.0.0.1 at port 3031 by default."},
                {"id":"unfetched","text":"The service listens on port 9443.","fetched_ok":false}
            ],
            "required_terms":["127.0.0.1","3031"],
            "fixture_response":"The service listens on loopback address 127.0.0.1 at port 3031 by default [config]."
        })).unwrap();
        assert!(
            verify_task(&task, &task_fixture_response(&task))
                .await
                .is_ok()
        );
        assert!(
            verify_task(&task, "The service listens on port 9443 [config].")
                .await
                .is_err()
        );
        assert!(
            verify_task(&task, "The service listens on port 3031 [unfetched].")
                .await
                .is_err()
        );
        assert!(
            verify_task(&task, "The service listens on port 3031 [invented].")
                .await
                .is_err()
        );
        assert!(
            verify_task(&task, "The service listens on port 3031.")
                .await
                .is_err()
        );
    }

    fn task_fixture_response(task: &Task) -> &str {
        match &task.verifier {
            Verifier::Research {
                fixture_response, ..
            } => fixture_response,
            _ => panic!("test task uses the research verifier"),
        }
    }
}
