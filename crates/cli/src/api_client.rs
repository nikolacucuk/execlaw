//! Versioned headless REST client used by `execlaw client`.

use crate::ClientOp;
use anyhow::{Context, bail};
use reqwest::{Client, Method, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const CLIENT_VERSION: u32 = 1;
const KEYRING_ACCOUNT: &str = "cli-refresh-token";

#[derive(Debug, Deserialize)]
struct ClientContract {
    version: u32,
    minimum_client_version: u32,
}

#[derive(Debug, Deserialize)]
struct TokenPair {
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Serialize)]
struct SendRequest<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resume_run_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_contract: Option<Value>,
}

/// Run one terminal-client operation.
pub fn run(operation: ClientOp) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_async(operation))
}

async fn run_async(operation: ClientOp) -> anyhow::Result<()> {
    match operation {
        ClientOp::EditorAdapter { server } => run_editor_adapter(server),
        ClientOp::Qualify {
            server,
            context_tokens,
        } => {
            anyhow::ensure!(
                (4_096..=262_144).contains(&context_tokens),
                "context tokens must be between 4096 and 262144"
            );
            let api = authenticated(&server).await?;
            let response = api
                .request(Method::POST, "/api/admin/inference/qualify")
                .json(&serde_json::json!({"context_tokens": context_tokens}))
                .send()
                .await
                .context("run model qualification")?;
            print_json(decode(response).await?)
        }
        ClientOp::Login { server, username } => {
            let password = rpassword::prompt_password("Controller password: ")?;
            let http = http_client()?;
            check_contract(&http, &server).await?;
            let response = http
                .post(join(&server, "/api/login"))
                .json(&serde_json::json!({"username": username, "admin_password": password}))
                .send()
                .await
                .context("login request failed")?;
            let value = decode(response).await?;
            if value.get("webauthn_required").and_then(Value::as_bool) == Some(true) {
                bail!(
                    "this account requires a passkey; complete WebAuthn in the SPA, then import a refresh token using `execlaw client import-refresh-token`"
                );
            }
            let tokens: TokenPair =
                serde_json::from_value(value).context("decode login response")?;
            save_refresh_token(&tokens.refresh_token)?;
            println!("Controller session saved in the OS keyring.");
            Ok(())
        }
        ClientOp::ImportRefreshToken => {
            let token = rpassword::prompt_password("Refresh token: ")?;
            if token.trim().is_empty() {
                bail!("refresh token is empty");
            }
            save_refresh_token(token.trim())?;
            println!("Refresh token saved in the OS keyring.");
            Ok(())
        }
        ClientOp::Logout => {
            let entry = keyring_entry()?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => println!("Controller session removed."),
                Err(error) => return Err(error).context("remove Controller session from keyring"),
            }
            Ok(())
        }
        ClientOp::Send {
            server,
            conversation_id,
            text,
            request_id,
            acceptance_criteria,
            optional_acceptance_criteria,
            required_artifacts,
            delivery_required,
            resume_run_id,
        } => {
            anyhow::ensure!(
                resume_run_id.is_some() || !text.trim().is_empty(),
                "--text is required unless --resume-run-id is provided"
            );
            anyhow::ensure!(
                resume_run_id.is_none() || text.trim().is_empty(),
                "--resume-run-id resumes saved input; omit --text"
            );
            anyhow::ensure!(
                resume_run_id.is_none()
                    || (acceptance_criteria.is_empty()
                        && optional_acceptance_criteria.is_empty()
                        && required_artifacts.is_empty()
                        && !delivery_required),
                "a resume must use the saved task contract; do not supply contract flags"
            );
            let completion_contract = build_completion_contract(
                &acceptance_criteria,
                &optional_acceptance_criteria,
                &required_artifacts,
                delivery_required,
            )?;
            let api = authenticated(&server).await?;
            let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let response = api
                .request(
                    Method::POST,
                    &format!("/api/chats/{}/messages", encode(&conversation_id)),
                )
                .header("Idempotency-Key", request_id)
                .json(&SendRequest {
                    text: &text,
                    resume_run_id: resume_run_id.as_deref(),
                    completion_contract,
                })
                .send()
                .await
                .context("send chat message")?;
            print_json(decode(response).await?)
        }
        ClientOp::Messages {
            server,
            conversation_id,
            after,
        } => {
            let api = authenticated(&server).await?;
            let url = format!(
                "{}/api/chats/{}/messages?before={}&limit=200",
                api.base,
                encode(&conversation_id),
                after.max(0)
            );
            print_json(
                decode(
                    api.request(Method::GET, &url)
                        .send()
                        .await
                        .context("read chat messages")?,
                )
                .await?,
            )
        }
        ClientOp::Stop {
            server,
            conversation_id,
        } => {
            let api = authenticated(&server).await?;
            let path = format!("/api/chats/{}/stop", encode(&conversation_id));
            print_json(
                decode(
                    api.request(Method::POST, &path)
                        .send()
                        .await
                        .context("cancel active turn")?,
                )
                .await?,
            )
        }
        ClientOp::Control {
            server,
            conversation_id,
            kind,
            text,
            request_id,
        } => {
            let api = authenticated(&server).await?;
            let path = format!("/api/chats/{}/controls", encode(&conversation_id));
            let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            print_json(
                decode(
                    api.request(Method::POST, &path)
                        .header("Idempotency-Key", request_id)
                        .json(&serde_json::json!({"kind":kind,"text":text}))
                        .send()
                        .await
                        .context("submit durable turn control")?,
                )
                .await?,
            )
        }
        ClientOp::Controls {
            server,
            conversation_id,
            after_created_at,
        } => {
            let api = authenticated(&server).await?;
            let path = format!(
                "/api/chats/{}/controls?after_created_at={}",
                encode(&conversation_id),
                after_created_at.max(0)
            );
            print_json(
                decode(
                    api.request(Method::GET, &path)
                        .send()
                        .await
                        .context("reconnect turn controls")?,
                )
                .await?,
            )
        }
        ClientOp::Approvals { server } => {
            let api = authenticated(&server).await?;
            print_json(
                decode(
                    api.request(Method::GET, "/api/admin/approvals")
                        .send()
                        .await
                        .context("list approvals")?,
                )
                .await?,
            )
        }
        ClientOp::RespondApproval {
            server,
            approval_id,
            approval_token,
            verb,
            reason,
        } => {
            let api = authenticated(&server).await?;
            let path = format!("/api/admin/approvals/{}/respond", encode(&approval_id));
            let body = serde_json::json!({"approval_token":approval_token,"verb":verb,"reason":reason,"allowed_topics":[]});
            print_json(
                decode(
                    api.request(Method::POST, &path)
                        .json(&body)
                        .send()
                        .await
                        .context("respond to approval")?,
                )
                .await?,
            )
        }
        ClientOp::DownloadArtifact {
            server,
            attachment_id,
            output,
        } => {
            let api = authenticated(&server).await?;
            let path = format!("/api/attachments/{}", encode(&attachment_id));
            let response = api
                .request(Method::GET, &path)
                .send()
                .await
                .context("download artifact")?;
            let mut response = response
                .error_for_status()
                .context("artifact download rejected")?;
            if response
                .content_length()
                .is_some_and(|length| length > 100 * 1024 * 1024)
            {
                bail!("artifact exceeds the 100 MiB terminal-client download limit");
            }
            let mut file = tokio::fs::File::create(&output)
                .await
                .with_context(|| format!("create {}", output.display()))?;
            let mut total = 0u64;
            while let Some(chunk) = response.chunk().await.context("read artifact stream")? {
                total = total.saturating_add(chunk.len() as u64);
                if total > 100 * 1024 * 1024 {
                    bail!("artifact exceeds the 100 MiB terminal-client download limit");
                }
                tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                    .await
                    .with_context(|| format!("write {}", output.display()))?;
            }
            tokio::io::AsyncWriteExt::flush(&mut file)
                .await
                .context("flush artifact file")?;
            println!("{} bytes written to {}", total, output.display());
            Ok(())
        }
        ClientOp::RegisterWorkspace { server, path } => {
            let api = authenticated(&server).await?;
            print_json(
                decode(
                    api.request(Method::POST, "/api/admin/workspaces")
                        .json(&serde_json::json!({"path":path}))
                        .send()
                        .await
                        .context("register workspace root")?,
                )
                .await?,
            )
        }
        ClientOp::WorkspaceRead {
            server,
            workspace_id,
            path,
        } => {
            let api = authenticated(&server).await?;
            let path = url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>();
            let route = format!(
                "/api/admin/workspaces/{}/files?path={path}",
                encode(&workspace_id)
            );
            print_json(
                decode(
                    api.request(Method::GET, &route)
                        .send()
                        .await
                        .context("read workspace file")?,
                )
                .await?,
            )
        }
        ClientOp::WorkspaceSearch {
            server,
            workspace_id,
            query,
        } => {
            let api = authenticated(&server).await?;
            let route = format!("/api/admin/workspaces/{}/search", encode(&workspace_id));
            print_json(
                decode(
                    api.request(Method::POST, &route)
                        .json(&serde_json::json!({"query":query}))
                        .send()
                        .await
                        .context("search workspace files")?,
                )
                .await?,
            )
        }
        ClientOp::WorkspaceCheckpoint {
            server,
            run_id,
            workspace_id,
        } => {
            let api = authenticated(&server).await?;
            let route = format!("/api/admin/runs/{}/workspace-checkpoints", encode(&run_id));
            print_json(
                decode(
                    api.request(Method::POST, &route)
                        .json(&serde_json::json!({"workspace_id":workspace_id}))
                        .send()
                        .await
                        .context("create workspace checkpoint")?,
                )
                .await?,
            )
        }
        ClientOp::WorkspaceDiff { server, run_id } => {
            let api = authenticated(&server).await?;
            let route = format!("/api/admin/runs/{}/workspace-diff", encode(&run_id));
            print_json(
                decode(
                    api.request(Method::GET, &route)
                        .send()
                        .await
                        .context("preview workspace diff")?,
                )
                .await?,
            )
        }
        ClientOp::WorkspaceApply {
            server,
            run_id,
            preview_hash,
            request_id,
        } => {
            let api = authenticated(&server).await?;
            let route = format!("/api/admin/runs/{}/workspace-apply", encode(&run_id));
            print_json(
                decode(
                    api.request(Method::POST, &route)
                        .header("Idempotency-Key", request_id)
                        .json(&serde_json::json!({"preview_hash":preview_hash}))
                        .send()
                        .await
                        .context("apply reviewed workspace diff")?,
                )
                .await?,
            )
        }
        ClientOp::WorkspaceRestore {
            server,
            run_id,
            apply_id,
            request_id,
        } => {
            let api = authenticated(&server).await?;
            let route = format!("/api/admin/runs/{}/workspace-restore", encode(&run_id));
            print_json(
                decode(
                    api.request(Method::POST, &route)
                        .header("Idempotency-Key", request_id)
                        .json(&serde_json::json!({"apply_id":apply_id}))
                        .send()
                        .await
                        .context("restore owned workspace changes")?,
                )
                .await?,
            )
        }
    }
}

fn build_completion_contract(
    criteria: &[String],
    optional_criteria: &[String],
    artifacts: &[String],
    delivery_required: bool,
) -> anyhow::Result<Option<Value>> {
    let parse_specs = |values: &[String],
                       label: &str,
                       seen: &mut std::collections::HashSet<String>|
     -> anyhow::Result<Vec<(String, String)>> {
        values
            .iter()
            .map(|value| {
                let (id, description) = value
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("{label} must use ID=DESCRIPTION form"))?;
                let id = id.trim();
                let description = description.trim();
                anyhow::ensure!(
                    !id.is_empty()
                        && id.len() <= 128
                        && !description.is_empty()
                        && description.len() <= 2_000,
                    "{label} needs a non-empty ID (max 128 bytes) and description (max 2000 bytes)"
                );
                anyhow::ensure!(seen.insert(id.to_owned()), "duplicate {label} ID '{id}'");
                Ok((id.to_owned(), description.to_owned()))
            })
            .collect()
    };

    let mut seen_criteria = std::collections::HashSet::new();
    let mut criteria = parse_specs(criteria, "acceptance criterion", &mut seen_criteria)?
        .into_iter()
        .map(|(id, description)| {
            serde_json::json!({
                "criterion_id": id,
                "description": description,
                "required": true
            })
        })
        .collect::<Vec<_>>();
    criteria.extend(
        parse_specs(
            optional_criteria,
            "acceptance criterion",
            &mut seen_criteria,
        )?
        .into_iter()
        .map(|(id, description)| {
            serde_json::json!({
                "criterion_id": id,
                "description": description,
                "required": false
            })
        }),
    );
    let mut seen_artifacts = std::collections::HashSet::new();
    let artifacts = parse_specs(artifacts, "required artifact", &mut seen_artifacts)?
        .into_iter()
        .map(|(id, description)| {
            serde_json::json!({
                "artifact_id": id,
                "description": description
            })
        })
        .collect::<Vec<_>>();

    if criteria.is_empty() && artifacts.is_empty() {
        anyhow::ensure!(
            !delivery_required,
            "--delivery-required needs at least one --acceptance-criterion or --required-artifact"
        );
        return Ok(None);
    }
    anyhow::ensure!(
        !artifacts.is_empty()
            || criteria
                .iter()
                .any(|criterion| criterion["required"] == true),
        "a completion contract needs at least one required criterion or artifact"
    );
    Ok(Some(serde_json::json!({
        "acceptance_criteria": criteria,
        "required_artifacts": artifacts,
        "delivery_required": delivery_required
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_request_includes_deterministic_task_contract() {
        let contract = build_completion_contract(
            &["tests=Focused tests pass".into()],
            &["optional=No severe findings".into()],
            &["artifact=Build report".into()],
            true,
        )
        .unwrap()
        .unwrap();
        let request = serde_json::to_value(SendRequest {
            text: "implement the task",
            resume_run_id: None,
            completion_contract: Some(contract),
        })
        .unwrap();
        assert_eq!(
            request["completion_contract"]["acceptance_criteria"][0]["criterion_id"],
            "tests"
        );
        assert_eq!(
            request["completion_contract"]["acceptance_criteria"][0]["required"],
            true
        );
        assert_eq!(
            request["completion_contract"]["acceptance_criteria"][1]["required"],
            false
        );
        assert_eq!(
            request["completion_contract"]["required_artifacts"][0]["artifact_id"],
            "artifact"
        );
        assert_eq!(request["completion_contract"]["delivery_required"], true);
    }

    #[test]
    fn send_request_omits_contract_when_no_verification_was_requested() {
        assert!(
            build_completion_contract(&[], &[], &[], false)
                .unwrap()
                .is_none()
        );
        assert!(build_completion_contract(&[], &[], &[], true).is_err());
        assert!(
            build_completion_contract(&[], &["optional=nice to have".into()], &[], false).is_err()
        );
    }

    #[test]
    fn resume_request_uses_saved_input_and_contract() {
        let request = serde_json::to_value(SendRequest {
            text: "",
            resume_run_id: Some("turn:conv-1:9"),
            completion_contract: None,
        })
        .unwrap();
        assert_eq!(request["text"], "");
        assert_eq!(request["resume_run_id"], "turn:conv-1:9");
        assert!(request.get("completion_contract").is_none());
    }

    #[test]
    fn task_contract_rejects_duplicate_or_malformed_requirements() {
        assert!(
            build_completion_contract(
                &["same=first".into(), "same=second".into()],
                &[],
                &[],
                false,
            )
            .is_err()
        );
        assert!(
            build_completion_contract(&["criterion without separator".into()], &[], &[], false)
                .is_err()
        );
        assert!(build_completion_contract(&[], &[], &["=missing id".into()], false).is_err());
    }

    #[test]
    fn editor_adapter_maps_completion_contract_to_headless_client_flags() {
        let arguments = vec![
            serde_json::json!("conv-1"),
            serde_json::json!("run a task"),
            serde_json::json!({
                "acceptance_criteria": [
                    {"criterion_id":"required", "description":"tests pass", "required":true},
                    {"criterion_id":"optional", "description":"stay under budget", "required":false}
                ],
                "required_artifacts": [
                    {"artifact_id":"report", "description":"test report"}
                ],
                "delivery_required": true
            }),
        ];
        let args =
            editor_command_args("execlaw.chat.send", "http://localhost", &arguments).unwrap();
        for expected in [
            "--acceptance-criterion",
            "required=tests pass",
            "--optional-acceptance-criterion",
            "optional=stay under budget",
            "--required-artifact",
            "report=test report",
            "--delivery-required",
        ] {
            assert!(
                args.iter().any(|argument| argument == expected),
                "{expected}"
            );
        }
        assert!(
            editor_contract_cli_args(&serde_json::json!({
                "required_artifacts":[{"artifact_id":"x","description":"y","required":false}]
            }))
            .is_err()
        );
    }
}

struct AuthenticatedClient {
    http: Client,
    base: String,
    access_token: String,
}

impl AuthenticatedClient {
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, join(&self.base, path))
            .bearer_auth(&self.access_token)
            .header("X-Execlaw-Client-Version", CLIENT_VERSION.to_string())
    }
}

async fn authenticated(server: &str) -> anyhow::Result<AuthenticatedClient> {
    let http = http_client()?;
    let base = server.trim_end_matches('/').to_owned();
    check_contract(&http, &base).await?;
    let entry = keyring_entry()?;
    let refresh_token = entry.get_password().map_err(|error| {
        anyhow::anyhow!(
            "no usable Controller refresh token in OS keyring; run `execlaw client login`: {error}"
        )
    })?;
    let response = http
        .post(join(&base, "/api/token/refresh"))
        .json(&serde_json::json!({"refresh_token":refresh_token}))
        .send()
        .await
        .context("refresh Controller session")?;
    let tokens: TokenPair =
        serde_json::from_value(decode(response).await?).context("decode refreshed session")?;
    entry
        .set_password(&tokens.refresh_token)
        .context("rotate Controller refresh token in OS keyring")?;
    Ok(AuthenticatedClient {
        http,
        base,
        access_token: tokens.access_token,
    })
}

fn http_client() -> anyhow::Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

async fn check_contract(http: &Client, server: &str) -> anyhow::Result<()> {
    let contract: ClientContract = http
        .get(join(server, "/api/client-contract"))
        .send()
        .await
        .context("read server client contract")?
        .error_for_status()
        .context("server client contract rejected")?
        .json()
        .await
        .context("decode server client contract")?;
    if contract.minimum_client_version > CLIENT_VERSION || contract.version > CLIENT_VERSION {
        bail!(
            "server client contract {} requires client version {}; this client supports version {}",
            contract.version,
            contract.minimum_client_version,
            CLIENT_VERSION
        );
    }
    Ok(())
}

async fn decode(response: Response) -> anyhow::Result<Value> {
    let status = response.status();
    let body = response.bytes().await.context("read API response")?;
    let value: Value = serde_json::from_slice(&body).context("decode API JSON response")?;
    if !status.is_success() {
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        bail!("HTTP {}: {message}", status.as_u16());
    }
    Ok(value)
}

fn save_refresh_token(token: &str) -> anyhow::Result<()> {
    keyring_entry()?
        .set_password(token)
        .context("save refresh token in OS keyring")?;
    Ok(())
}

fn keyring_entry() -> anyhow::Result<keyring::Entry> {
    Ok(keyring::Entry::new("execlaw", KEYRING_ACCOUNT)?)
}

fn join(base: &str, path: &str) -> String {
    format!(
        "{}{}",
        base.trim_end_matches('/'),
        if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        }
    )
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn print_json(value: Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// Serve a bounded, command-allowlisted LSP `workspace/executeCommand` adapter.
/// The adapter invokes only this CLI's authenticated REST operations; it does
/// not expose shell execution or a second tool authority path.
pub fn run_editor_adapter(server: String) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};

    fn read_message(reader: &mut impl BufRead) -> anyhow::Result<Option<Value>> {
        let mut content_length = None;
        let mut total_header_bytes = 0usize;
        loop {
            let mut line = String::new();
            let count = reader.read_line(&mut line)?;
            if count == 0 {
                return Ok(None);
            }
            total_header_bytes = total_header_bytes.saturating_add(count);
            if total_header_bytes > 4096 {
                bail!("editor protocol header exceeds 4096 bytes");
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    let length: usize = value.trim().parse().context("invalid Content-Length")?;
                    if length > 1024 * 1024 {
                        bail!("editor protocol frame exceeds 1 MiB");
                    }
                    content_length = Some(length);
                }
            }
        }
        let length = content_length.context("missing Content-Length")?;
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body)?;
        Ok(Some(
            serde_json::from_slice(&body).context("invalid editor protocol JSON")?,
        ))
    }

    fn write_message(writer: &mut impl Write, value: &Value) -> anyhow::Result<()> {
        let body = serde_json::to_vec(value)?;
        if body.len() > 1024 * 1024 {
            bail!("editor protocol response exceeds 1 MiB");
        }
        write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
        writer.write_all(&body)?;
        writer.flush()?;
        Ok(())
    }

    let input = std::io::stdin();
    let output = std::io::stdout();
    let mut reader = std::io::BufReader::new(input.lock());
    let mut writer = std::io::BufWriter::new(output.lock());
    loop {
        let Some(request) = read_message(&mut reader)? else {
            break;
        };
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if method == "exit" {
            break;
        }
        if method == "initialized" || method == "$/setTrace" {
            continue;
        }
        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc":"2.0","id":id,
                "result":{
                    "capabilities":{"executeCommandProvider":{"commands":[
                        "execlaw.chat.send","execlaw.chat.messages","execlaw.chat.cancel",
                        "execlaw.chat.control","execlaw.approvals.list","execlaw.approvals.respond",
                        "execlaw.artifact.download","execlaw.workspace.read","execlaw.workspace.search",
                        "execlaw.workspace.checkpoint","execlaw.workspace.diff","execlaw.workspace.apply","execlaw.workspace.restore"
                    ]}},
                    "serverInfo":{"name":"execlaw-client","version":CLIENT_VERSION.to_string()}
                }
            }),
            "shutdown" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":null}),
            "workspace/executeCommand" => {
                let params = request.get("params").cloned().unwrap_or(Value::Null);
                let command = params
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let arguments = params
                    .get("arguments")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                match editor_command_args(command, &server, &arguments) {
                    Ok(arguments) => match std::env::current_exe()
                        .and_then(|exe| std::process::Command::new(exe).args(arguments).output())
                    {
                        Ok(output) if output.status.success() => {
                            let result = serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|_| serde_json::json!({"text":String::from_utf8_lossy(&output.stdout)}));
                            serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
                        }
                        Ok(output) => {
                            serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":String::from_utf8_lossy(&output.stderr).chars().take(2048).collect::<String>()}})
                        }
                        Err(error) => {
                            serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32002,"message":error.to_string()}})
                        }
                    },
                    Err(error) => {
                        serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":error}})
                    }
                }
            }
            _ if id.is_some() => {
                serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}})
            }
            _ => continue,
        };
        write_message(&mut writer, &response)?;
    }
    Ok(())
}

fn editor_command_args(
    command: &str,
    server: &str,
    arguments: &[Value],
) -> Result<Vec<String>, String> {
    let string = |index: usize, label: &str| -> Result<String, String> {
        arguments
            .get(index)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("argument {label} must be a string"))
    };
    let integer = |index: usize, label: &str| -> Result<String, String> {
        arguments
            .get(index)
            .and_then(Value::as_i64)
            .map(|value| value.to_string())
            .ok_or_else(|| format!("argument {label} must be an integer"))
    };
    let mut args = vec!["client".into()];
    match command {
        "execlaw.chat.send" => {
            args.extend([
                "send".into(),
                "--server".into(),
                server.into(),
                "--conversation-id".into(),
                string(0, "conversation_id")?,
                "--text".into(),
                string(1, "text")?,
                "--request-id".into(),
                uuid::Uuid::new_v4().to_string(),
            ]);
            if let Some(contract) = arguments.get(2) {
                args.extend(editor_contract_cli_args(contract)?);
            }
        }
        "execlaw.chat.messages" => args.extend([
            "messages".into(),
            "--server".into(),
            server.into(),
            "--conversation-id".into(),
            string(0, "conversation_id")?,
            "--after".into(),
            integer(1, "after")?,
        ]),
        "execlaw.chat.cancel" => args.extend([
            "stop".into(),
            "--server".into(),
            server.into(),
            "--conversation-id".into(),
            string(0, "conversation_id")?,
        ]),
        "execlaw.chat.control" => args.extend([
            "control".into(),
            "--server".into(),
            server.into(),
            "--conversation-id".into(),
            string(0, "conversation_id")?,
            "--kind".into(),
            string(1, "kind")?,
            "--text".into(),
            string(2, "text")?,
            "--request-id".into(),
            uuid::Uuid::new_v4().to_string(),
        ]),
        "execlaw.approvals.list" => {
            args.extend(["approvals".into(), "--server".into(), server.into()])
        }
        "execlaw.approvals.respond" => args.extend([
            "respond-approval".into(),
            "--server".into(),
            server.into(),
            "--approval-id".into(),
            string(0, "approval_id")?,
            "--approval-token".into(),
            string(1, "approval_token")?,
            "--verb".into(),
            string(2, "verb")?,
        ]),
        "execlaw.artifact.download" => args.extend([
            "download-artifact".into(),
            "--server".into(),
            server.into(),
            "--attachment-id".into(),
            string(0, "attachment_id")?,
            "--output".into(),
            string(1, "output")?,
        ]),
        "execlaw.workspace.read" => args.extend([
            "workspace-read".into(),
            "--server".into(),
            server.into(),
            "--workspace-id".into(),
            string(0, "workspace_id")?,
            "--path".into(),
            string(1, "path")?,
        ]),
        "execlaw.workspace.search" => args.extend([
            "workspace-search".into(),
            "--server".into(),
            server.into(),
            "--workspace-id".into(),
            string(0, "workspace_id")?,
            "--query".into(),
            string(1, "query")?,
        ]),
        "execlaw.workspace.checkpoint" => args.extend([
            "workspace-checkpoint".into(),
            "--server".into(),
            server.into(),
            "--run-id".into(),
            string(0, "run_id")?,
            "--workspace-id".into(),
            string(1, "workspace_id")?,
        ]),
        "execlaw.workspace.diff" => args.extend([
            "workspace-diff".into(),
            "--server".into(),
            server.into(),
            "--run-id".into(),
            string(0, "run_id")?,
        ]),
        "execlaw.workspace.apply" => args.extend([
            "workspace-apply".into(),
            "--server".into(),
            server.into(),
            "--run-id".into(),
            string(0, "run_id")?,
            "--preview-hash".into(),
            string(1, "preview_hash")?,
            "--request-id".into(),
            uuid::Uuid::new_v4().to_string(),
        ]),
        "execlaw.workspace.restore" => args.extend([
            "workspace-restore".into(),
            "--server".into(),
            server.into(),
            "--run-id".into(),
            string(0, "run_id")?,
            "--apply-id".into(),
            string(1, "apply_id")?,
            "--request-id".into(),
            uuid::Uuid::new_v4().to_string(),
        ]),
        _ => return Err(format!("command '{command}' is not allowed")),
    }
    Ok(args)
}

fn editor_contract_cli_args(contract: &Value) -> Result<Vec<String>, String> {
    let object = contract
        .as_object()
        .ok_or_else(|| "completion contract must be an object".to_owned())?;
    let mut args = Vec::new();
    for (field, id_field, flag, optional_flag) in [
        (
            "acceptance_criteria",
            "criterion_id",
            "--acceptance-criterion",
            Some("--optional-acceptance-criterion"),
        ),
        (
            "required_artifacts",
            "artifact_id",
            "--required-artifact",
            None,
        ),
    ] {
        let Some(values) = object.get(field) else {
            continue;
        };
        let values = values
            .as_array()
            .ok_or_else(|| format!("{field} must be an array"))?;
        for value in values {
            let entry = value
                .as_object()
                .ok_or_else(|| format!("each {field} entry must be an object"))?;
            let id = entry
                .get(id_field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("each {field} entry needs string {id_field}"))?;
            let description = entry
                .get("description")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("each {field} entry needs a string description"))?;
            let required = match entry.get("required") {
                None => true,
                Some(Value::Bool(required)) => *required,
                Some(_) => return Err(format!("{field} required must be a boolean")),
            };
            let selected_flag = if required { Some(flag) } else { optional_flag };
            let Some(selected_flag) = selected_flag else {
                return Err("required artifacts cannot be optional".into());
            };
            args.push(selected_flag.to_owned());
            args.push(format!("{id}={description}"));
        }
    }
    let delivery_required = match object.get("delivery_required") {
        None => false,
        Some(Value::Bool(required)) => *required,
        Some(_) => return Err("delivery_required must be a boolean".into()),
    };
    if delivery_required {
        args.push("--delivery-required".into());
    }
    Ok(args)
}
