//! Offline author checks for plugin projects and upgrades.

use crate::HookRegistry;
use execlaw_plugin_sdk::PluginManifest;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectReport {
    pub plugin_id: String,
    pub runtime_tier: String,
    pub tool_count: usize,
    pub case_count: usize,
    pub declared_capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ConformanceFixture {
    cases: Vec<ConformanceCase>,
    lifecycle: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ConformanceCase {
    tool_name: String,
    arguments: Value,
    result: Value,
    #[serde(default)]
    requested_capabilities: Vec<String>,
}

/// Validate a project offline using the production manifest and schema loader.
pub fn check_project(root: &Path) -> anyhow::Result<ProjectReport> {
    let manifest_path = root.join("plugin.toml");
    let source = std::fs::read_to_string(&manifest_path)
        .map_err(|error| anyhow::anyhow!("read {}: {error}", manifest_path.display()))?;
    let manifest = PluginManifest::parse(&source)
        .map_err(|error| anyhow::anyhow!("manifest invalid: {error}"))?;
    HookRegistry::new()
        .enable_with_stage(&manifest, Some(root))
        .map_err(|error| anyhow::anyhow!("host registration contract failed: {error}"))?;

    if let Some(runtime) = &manifest.runtime {
        let local_entry = runtime.source.as_ref().or_else(|| {
            runtime.executable.as_ref().filter(|value| {
                value.contains('/') || value.contains('\\') || value.starts_with('.')
            })
        });
        if let Some(entry) = local_entry {
            let path = safe_project_path(root, entry)?;
            if !path.is_file() {
                anyhow::bail!("runtime entrypoint is missing: {entry}");
            }
        }
    }

    let fixture_path = root.join("tests/conformance.json");
    let fixture: ConformanceFixture = serde_json::from_slice(
        &std::fs::read(&fixture_path)
            .map_err(|error| anyhow::anyhow!("read tests/conformance.json: {error}"))?,
    )
    .map_err(|error| anyhow::anyhow!("invalid conformance fixture: {error}"))?;
    if fixture.cases.is_empty() {
        anyhow::bail!("conformance fixture must contain at least one case");
    }
    let lifecycle: BTreeSet<_> = fixture.lifecycle.iter().map(String::as_str).collect();
    for phase in ["install", "enable", "call", "disable", "upgrade"] {
        if !lifecycle.contains(phase) {
            anyhow::bail!("conformance fixture is missing lifecycle phase '{phase}'");
        }
    }

    let tools: HashMap<_, _> = manifest
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let mut capabilities = BTreeSet::new();
    for case in &fixture.cases {
        let tool = tools.get(case.tool_name.as_str()).ok_or_else(|| {
            anyhow::anyhow!(
                "fixture names undeclared tool '{}'; add it to plugin.toml",
                case.tool_name
            )
        })?;
        let declared: BTreeSet<_> = tool
            .required_capabilities
            .iter()
            .map(String::as_str)
            .collect();
        for capability in &case.requested_capabilities {
            if !declared.contains(capability.as_str()) {
                anyhow::bail!(
                    "fixture requests undeclared authority '{}' for tool '{}'; declare it in plugin.toml or remove the request",
                    capability,
                    case.tool_name
                );
            }
        }
        capabilities.extend(tool.required_capabilities.iter().cloned());
        validate_fixture_schema(root, tool.schema.as_deref(), &case.arguments, "arguments")?;
        validate_fixture_schema(root, tool.result_schema.as_deref(), &case.result, "result")?;
    }

    Ok(ProjectReport {
        plugin_id: manifest.plugin.id,
        runtime_tier: manifest
            .runtime
            .as_ref()
            .map_or_else(|| "host-declared".into(), |runtime| runtime.tier.clone()),
        tool_count: manifest.tools.len(),
        case_count: fixture.cases.len(),
        declared_capabilities: capabilities.into_iter().collect(),
    })
}

/// Execute fixture calls through the declared runtime using only mock inputs.
pub async fn run_runtime_cases(root: &Path) -> anyhow::Result<()> {
    let manifest_source = std::fs::read_to_string(root.join("plugin.toml"))?;
    let manifest = PluginManifest::parse(&manifest_source)?;
    let fixture: ConformanceFixture =
        serde_json::from_slice(&std::fs::read(root.join("tests/conformance.json"))?)?;
    let runtime = manifest
        .runtime
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("a runtime is required for executable conformance cases"))?;
    match runtime.parsed_tier() {
        Some(execlaw_plugin_sdk::manifest::RuntimeTier::Script) => {
            let source = runtime
                .source
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("script runtime is missing source"))?;
            let source = safe_project_path(root, source)?;
            let plugin = execlaw_script::ScriptPlugin::from_file(
                &manifest.plugin.id,
                &source,
                &execlaw_script::ScriptEngine::new(),
            )?;
            for case in fixture.cases {
                let result = plugin
                    .tool_call(&case.tool_name, case.arguments, serde_json::Map::new())
                    .await?;
                if result != case.result {
                    anyhow::bail!(
                        "script runtime case '{}' returned {result}, expected {}",
                        case.tool_name,
                        case.result
                    );
                }
            }
        }
        Some(execlaw_plugin_sdk::manifest::RuntimeTier::Subprocess) => {
            let executable = runtime
                .executable
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("subprocess runtime is missing executable"))?;
            let executable_path = if executable.contains('/')
                || executable.contains('\\')
                || executable.starts_with('.')
            {
                safe_project_path(root, executable)?
                    .to_string_lossy()
                    .into_owned()
            } else {
                executable.to_owned()
            };
            let plugin = crate::SubprocessPlugin::spawn(
                crate::SubprocessSpec {
                    plugin_id: manifest.plugin.id.clone(),
                    executable: executable_path,
                    expected_sha256: None,
                    args: runtime.args.clone(),
                    cwd: Some(root.to_path_buf()),
                },
                None,
            )
            .await
            .map_err(|error| anyhow::anyhow!("spawn conformance plugin: {error}"))?;
            for case in fixture.cases {
                let result = plugin
                    .call(
                        "tool.call",
                        serde_json::json!({
                            "name": case.tool_name,
                            "arguments": case.arguments,
                        }),
                    )
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!("subprocess conformance call failed: {error}")
                    })?;
                if result != case.result {
                    plugin.shutdown().await;
                    anyhow::bail!(
                        "subprocess runtime case returned {result}, expected {}",
                        case.result
                    );
                }
            }
            plugin.shutdown().await;
        }
        Some(execlaw_plugin_sdk::manifest::RuntimeTier::Wasm) => {
            #[cfg(feature = "wasm-trial")]
            {
                let source = runtime
                    .source
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("wasm runtime is missing source"))?;
                let source = safe_project_path(root, source)?;
                let plugin = crate::WasmPlugin::load(&source)
                    .map_err(|error| anyhow::anyhow!("load Wasm conformance plugin: {error}"))?;
                for case in fixture.cases {
                    let result = plugin.transform(&case.arguments).map_err(|error| {
                        anyhow::anyhow!("Wasm conformance call failed: {error}")
                    })?;
                    if result != case.result {
                        anyhow::bail!(
                            "Wasm runtime case '{}' returned {result}, expected {}",
                            case.tool_name,
                            case.result
                        );
                    }
                }
            }
            #[cfg(not(feature = "wasm-trial"))]
            anyhow::bail!("Wasm runtime trial requires the wasm-trial feature");
        }
        None => anyhow::bail!("unsupported plugin runtime tier '{}'", runtime.tier),
    }
    Ok(())
}

/// Reject an upgrade that expands tool authority or relaxes a trust floor.
pub fn check_upgrade(previous: &PluginManifest, candidate: &PluginManifest) -> anyhow::Result<()> {
    if previous.plugin.id != candidate.plugin.id {
        anyhow::bail!(
            "upgrade plugin id changed from '{}' to '{}'",
            previous.plugin.id,
            candidate.plugin.id
        );
    }
    let previous_version = semver::Version::parse(&previous.plugin.version)
        .map_err(|error| anyhow::anyhow!("previous plugin version is not semver: {error}"))?;
    let candidate_version = semver::Version::parse(&candidate.plugin.version)
        .map_err(|error| anyhow::anyhow!("candidate plugin version is not semver: {error}"))?;
    if candidate_version <= previous_version {
        anyhow::bail!(
            "upgrade version {candidate_version} must exceed installed version {previous_version}"
        );
    }
    for old_tool in &previous.tools {
        let Some(new_tool) = candidate
            .tools
            .iter()
            .find(|tool| tool.name == old_tool.name)
        else {
            continue;
        };
        let old_caps: BTreeSet<_> = old_tool.required_capabilities.iter().collect();
        for capability in &new_tool.required_capabilities {
            if !old_caps.contains(capability) {
                anyhow::bail!(
                    "upgrade adds authority '{capability}' to tool '{}'",
                    old_tool.name
                );
            }
        }
        if trust_rank(new_tool.trust_floor.as_deref()) < trust_rank(old_tool.trust_floor.as_deref())
        {
            anyhow::bail!(
                "upgrade lowers the trust floor for tool '{}'",
                old_tool.name
            );
        }
        if old_tool.effect_contract != new_tool.effect_contract {
            anyhow::bail!(
                "upgrade changes the effect contract for tool '{}'",
                old_tool.name
            );
        }
    }
    Ok(())
}

fn validate_fixture_schema(
    root: &Path,
    schema: Option<&str>,
    value: &Value,
    label: &str,
) -> anyhow::Result<()> {
    let Some(schema) = schema else {
        return Ok(());
    };
    let path = safe_project_path(root, schema)?;
    let schema_json: Value = serde_json::from_slice(&std::fs::read(&path)?)
        .map_err(|error| anyhow::anyhow!("parse schema {schema}: {error}"))?;
    let validator = jsonschema::validator_for(&schema_json)
        .map_err(|error| anyhow::anyhow!("invalid schema {schema}: {error}"))?;
    if let Err(error) = validator.validate(value) {
        anyhow::bail!("fixture {label} does not satisfy {schema}: {error}");
    }
    Ok(())
}

fn safe_project_path(root: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!("project path escapes plugin root: {relative}");
    }
    Ok(root.join(path))
}

fn trust_rank(floor: Option<&str>) -> u8 {
    match floor {
        Some("Controller") => 6,
        Some("Delegated") => 5,
        Some("KnownTrusted") => 4,
        Some("KnownLimited") => 3,
        Some("UnknownPending") => 2,
        Some("Blocked") => 1,
        None => 0,
        Some(_) => u8::MAX,
    }
}

/// Generate a minimal script- or subprocess-tier project without overwriting files.
pub fn generate_project(root: &Path, plugin_id: &str, tier: &str) -> anyhow::Result<()> {
    if !matches!(tier, "script" | "subprocess") {
        anyhow::bail!("runtime tier must be 'script' or 'subprocess'");
    }
    if plugin_id.is_empty()
        || !plugin_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        anyhow::bail!("plugin id must contain only ASCII letters, digits, '-' or '_'");
    }
    std::fs::create_dir_all(root.join("schemas"))?;
    std::fs::create_dir_all(root.join("tests"))?;
    let runtime = if tier == "script" {
        "[runtime]\ntier = \"script\"\nsource = \"main.rhai\"\n"
    } else {
        "[runtime]\ntier = \"subprocess\"\nexecutable = \"python\"\nargs = [\"main.py\"]\n"
    };
    let manifest = format!(
        "[plugin]\nid = \"{plugin_id}\"\nname = \"{plugin_id}\"\nversion = \"0.1.0\"\n\n[compatibility]\nhost_api = \"^1.0.0\"\nrequired_features = [\"jsonrpc.line.v1\", \"tool.schema.validation.v1\", \"tool.result.schema.v1\"]\n\n[[tools]]\nname = \"{plugin_id}.echo\"\nschema = \"schemas/echo.json\"\nresult_schema = \"schemas/echo-result.json\"\nrequired_capabilities = []\n\n{runtime}"
    );
    write_new(&root.join("plugin.toml"), manifest.as_bytes())?;
    write_new(&root.join("schemas/echo.json"), br#"{"type":"object","required":["value"],"properties":{"value":{"type":"string"}},"additionalProperties":false}"#)?;
    write_new(&root.join("schemas/echo-result.json"), br#"{"type":"object","required":["value"],"properties":{"value":{"type":"string"}},"additionalProperties":false}"#)?;
    let fixture = format!(
        "{{\"lifecycle\":[\"install\",\"enable\",\"call\",\"disable\",\"upgrade\"],\"cases\":[{{\"tool_name\":\"{plugin_id}.echo\",\"arguments\":{{\"value\":\"hello\"}},\"result\":{{\"value\":\"hello\"}},\"requested_capabilities\":[]}}]}}"
    );
    write_new(&root.join("tests/conformance.json"), fixture.as_bytes())?;
    let entry = if tier == "script" {
        "fn tool_call(name, args, oauth_tokens) { #{ \"value\": args.value } }\n"
    } else {
        "import json, sys\nfor line in sys.stdin:\n    req = json.loads(line)\n    result = {\"value\": req[\"params\"][\"arguments\"][\"value\"]}\n    print(json.dumps({\"id\": req[\"id\"], \"result\": result}), flush=True)\n"
    };
    write_new(
        &root.join(if tier == "script" {
            "main.rhai"
        } else {
            "main.py"
        }),
        entry.as_bytes(),
    )?;
    write_new(&root.join("README.md"), b"Run `execlaw-plugin-conformance check .` before sharing. This offline check does not install or enable the plugin.\n")?;
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            anyhow::anyhow!("create {} without overwriting: {error}", path.display())
        })?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_script_and_subprocess_projects_pass_the_offline_contracts() {
        for tier in ["script", "subprocess"] {
            let directory = tempfile::tempdir().unwrap();
            let project = directory.path().join("sample-plugin");
            generate_project(&project, "sample-plugin", tier).unwrap();
            let report = check_project(&project).unwrap();
            assert_eq!(report.plugin_id, "sample-plugin");
            assert_eq!(report.runtime_tier, tier);
            assert_eq!(report.case_count, 1);
            assert!(generate_project(&project, "sample-plugin", tier).is_err());
        }
    }

    #[tokio::test]
    async fn generated_script_runtime_executes_against_the_mock_host_fixture() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("sample-plugin");
        generate_project(&project, "sample-plugin", "script").unwrap();
        run_runtime_cases(&project).await.unwrap();
    }

    #[tokio::test]
    async fn generated_subprocess_runtime_executes_against_the_mock_host_fixture() {
        if !std::process::Command::new("python")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            // no Python runtime on this host
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("sample-plugin");
        generate_project(&project, "sample-plugin", "subprocess").unwrap();
        run_runtime_cases(&project).await.unwrap();
    }

    #[test]
    fn fixture_cannot_request_authority_not_declared_by_its_tool() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("sample-plugin");
        generate_project(&project, "sample-plugin", "script").unwrap();
        let fixture_path = project.join("tests/conformance.json");
        let fixture: Value =
            serde_json::from_slice(&std::fs::read(&fixture_path).unwrap()).unwrap();
        let mut fixture = fixture;
        fixture["cases"][0]["requested_capabilities"] = serde_json::json!(["workspace.write"]);
        std::fs::write(&fixture_path, serde_json::to_vec(&fixture).unwrap()).unwrap();
        assert!(
            check_project(&project)
                .unwrap_err()
                .to_string()
                .contains("undeclared authority")
        );
    }

    #[test]
    fn invalid_schema_fails_with_the_production_host_diagnostic() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("sample-plugin");
        generate_project(&project, "sample-plugin", "script").unwrap();
        std::fs::write(project.join("schemas/echo.json"), b"not-json").unwrap();
        assert!(
            check_project(&project)
                .unwrap_err()
                .to_string()
                .contains("schema")
        );
    }

    #[test]
    fn upgrade_rejects_authority_expansion_and_relaxed_trust_floor() {
        let old = PluginManifest::parse("[plugin]\nid=\"sample\"\nname=\"Sample\"\nversion=\"1.0.0\"\n[[tools]]\nname=\"sample.write\"\nrequired_capabilities=[\"workspace.write\"]\ntrust_floor=\"Controller\"\n").unwrap();
        let added = PluginManifest::parse("[plugin]\nid=\"sample\"\nname=\"Sample\"\nversion=\"1.1.0\"\n[[tools]]\nname=\"sample.write\"\nrequired_capabilities=[\"workspace.write\",\"network.approved\"]\ntrust_floor=\"Controller\"\n").unwrap();
        assert!(
            check_upgrade(&old, &added)
                .unwrap_err()
                .to_string()
                .contains("adds authority")
        );
        let relaxed = PluginManifest::parse("[plugin]\nid=\"sample\"\nname=\"Sample\"\nversion=\"1.1.0\"\n[[tools]]\nname=\"sample.write\"\nrequired_capabilities=[\"workspace.write\"]\ntrust_floor=\"KnownTrusted\"\n").unwrap();
        assert!(
            check_upgrade(&old, &relaxed)
                .unwrap_err()
                .to_string()
                .contains("lowers the trust floor")
        );
    }
}
