//! Reproducible real-task scoring. Expected answers and verifiers are loaded
//! from the suite directory, never copied into the model's prompt/workspace.

use anyhow::{Context, bail};
use execlaw_inference_api::{ChatMessage, ChatRequest, InferenceClient, ModelId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

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
}

#[derive(Debug, Deserialize)]
struct Source {
    id: String,
    text: String,
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
    if suite
        .tasks
        .iter()
        .any(|task| matches!(&task.verifier, Verifier::Coding { .. }))
        && !offline_fixture
        && !allow_executing_generated_code
    {
        bail!(
            "coding tasks execute model-produced code; pass --allow-executing-generated-code to acknowledge"
        );
    }
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
                user_prompt.push_str(&format!("[{}] {}\n", source.id, source.text));
            }
            user_prompt.push_str("Cite source IDs exactly as [id].");
        }
        Verifier::Coding { output_file, .. } => user_prompt.push_str(&format!(
            "\nReturn only the complete replacement contents for {}.",
            output_file.display()
        )),
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
            let cited = sources
                .iter()
                .any(|source| output.contains(&format!("[{}]", source.id)));
            check_terms(output, required_terms).and_then(|()| {
                if cited {
                    Ok(())
                } else {
                    Err("answer omitted a source citation".to_owned())
                }
            })
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
    }
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

    #[test]
    fn wilson_interval_contains_observed_rate() {
        let (rate, low, high) = wilson(7, 10);
        assert_eq!(rate, 0.7);
        assert!(low < rate && high > rate);
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
}
