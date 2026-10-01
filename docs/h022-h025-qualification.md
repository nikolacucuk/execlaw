# H022-H025 qualification and recovery notes

This is an operator and coding-agent runbook for the acceptance gates in the
[roadmap](llm-harness-roadmap.md#enhancement-022). The
[implementation ledger](implementation-plan.md) owns current status. As of
2026-09-30, **H022-H025 are Partial**. Passing focused tests or reaching a
model endpoint does not close the remaining real-task, cross-path, installed
release, and live supported-endpoint gates.

## First establish which installation is under test

On this Windows laptop, the SPA at `http://127.0.0.1:5174` proxies to the
existing API at `http://127.0.0.1:3031`. Check both `/api/health` endpoints,
then inspect the API process command line and binary path before attributing
results to new source. During the 2026-09-30 qualification, that API was an
older `target-local-ui-verification/debug/execlaw.exe`; tests of the current
source used a copied debug executable, a disposable database, and port 3032.
Do not present results from one installation as proof for the other. Keep
test databases and keys separate from `~/.execlaw` and do not erase user
state.

For a model on TrueNAS, configure the external Standard backend with the
TrueNAS address, not the laptop's Docker hostname. In this environment,
`host.docker.internal` resolves to `192.168.1.84` (the laptop); Ollama was at
`192.168.1.76:30068`. The observed working OpenAI-compatible base was
`http://192.168.1.76:30068/v1`, with model
`fredrezones55/Qwen3.6-35B-A3B-APEX:I-Mini`. Approve the actual server IP
under **LocalInference** with the narrow `192.168.1.76/32` scope. A
PrivateIntegration approval does not authorize inference. Recheck the IP and
model identifier for each installation; these observations are not defaults.

Check reachability and exact model identity before a qualification run:

```powershell
curl.exe http://127.0.0.1:5174/api/health
curl.exe http://127.0.0.1:3031/api/health
curl.exe http://192.168.1.76:30068/api/version
curl.exe http://192.168.1.76:30068/v1/models
```

Model listing alone proves neither a successful chat nor enough context. The
2026-09-30 isolated run used the external backend's `model_spec_json` with
`binary_hint: "ollama"`, `reasoning_effort: "none"`, and
`context_tokens: 32768`. Native `/api/chat` then sent `options.num_ctx=32768`;
qualification of an effective 16K prompt window passed, including text,
streaming, tool calls, and three structured-output probes. The earlier
OpenAI-compatible configuration had a blank response at a 64-token output
limit, and a 4K qualified window left no output room under the old reserve
calculation. Requalify after changing model metadata, endpoint, backend,
context, or thinking setting. Record the model tag and `api/show` template
hash so a later model update cannot be mistaken for a code regression.

The isolated Controller API uses `POST /api/admin/inference/qualify` for the
configured backend. It requires a Controller session. Never put session
tokens in documentation, logs, or test fixtures. A cold model can take longer
than the first probe timeout; separate startup latency from a failed
capability probe. The production UI requires its own authenticated check;
the isolated API probe did not verify the logged-in SPA.

## Acceptance evidence still required

| Item | Demonstrated on 2026-09-30 | Required before Complete |
|---|---|---|
| H022 | Disposable real-model headless and editor-style API requests, routine fire, and agent runs reached VerifiedComplete. A real plugin tool and research job each produced a run-owned attachment that passed artifact and Controller review checks. Fabricated attachments are rejected. | Prove the actual headless CLI and editor adapter processes, rather than API-emulated requests, and rerun the full acceptance matrix on a settled source build. The editor-style request did not exercise VS Code. |
| H023 | An isolated debug host kill resumed held inference; a child kill regression reclaimed one model attempt. An expired-budget startup loop was fixed and a disposable restart terminalized the run once. Runner namespace isolation, admission accounting, prompt replay identity, and per-group spawn gates were changed; a real runner turn passed before the kill attempt. | Resume a runner turn successfully after a process kill on the final binary, then cover streaming, routine, tool effects, paired commits, and child joins with effect counts. The held runner kill was observed, but the run expired during subsequent builds before successful replay. The latest spawn-gate change has not been live-qualified. |
| H024 | Same-key retry returned the original run. An installed debug copy killed after a disposable sink accepted a send restarted to Unknown with one attempt and one sink effect. | Build and install a release, repeat the transport/sink kill matrix with supported idempotency or status lookup, and review reconciliation decisions. Windows Application Control error 4551 blocked Cargo-generated release build executables; the alternative Docker SQLCipher release build was paused before producing an image when host memory became scarce. |
| H025 | TrueNAS inference passed scoped approval and revocation checks. Live public OpenMeteo plugin and research fetch succeeded. MCP private approval, denial, and redirect probes showed no requests to the forbidden listener. Focused public automation and Google OAuth policy checks passed. | Run the complete live cross-adapter adversarial matrix, including DNS rebinding, mixed answers, mapped addresses, proxies, and redirects on supported endpoints. The full script was not rerun on the final source. No authenticated external OAuth/MCP service or authenticated SPA/browser result was available. |

H022 proof must point to a run-owned artifact whose file still matches its
recorded hash, a run-scoped delivered outbox event, or a Controller audit
attestation for the exact review action. A model response is not a
deterministic verifier. A claimed attachment identifier is not proof that an
artifact was produced. Check report state after reopening the database, since
proof can expire or become invalid. See `crates/core/src/completion_evidence.rs`,
`runs.rs`, and `agents.rs`.

For H023, test kill cuts before inference, during inference, after a model
checkpoint, during a tool effect, after a paired tool commit, and during child
join. For each cut, retain the same durable run identity, check attempt
transitions, count user/model/tool events, verify every `tool_use` has one
paired `tool_result`, and count actual external effects. The focused child
regression is
`cargo test -p execlaw-server killed_child_inference_reclaims_once_and_publishes_one_artifact`.
It covers one cut, not the entire matrix. Runner containers and volumes now
include a database-scoped installation identifier so disposable qualification
does not sweep the live installation's runner resources. Still verify PID,
command line, database, container name, and Docker context before any kill.

The earlier repeated startup recovery for
`turn:qa-real-h022-review-1790791531633:1` exposed a budget check gap.
Startup now atomically terminalizes expired runs before dispatch; focused
core/server regressions and a disposable restart showed one terminalization
instead of a hot retry loop. A separate held runner process kill reached
persisted attempt 1, but its wall-clock budget expired during rebuilds before
the final replay fix could be qualified. Preserve disposable databases for
diagnosis; do not copy them over operator data.

For H024, an accepted send followed by a lost acknowledgement is ambiguous
when the sink has no deduplication or lookup. The expected state is **Unknown**,
with no automatic second send. The operator must resolve it with evidence.
`lease_reclaimed` records recovery of an expired relay lease; it must not be
misread as another `send_requested`. Count sink effects independently of
outbox rows and use the same caller idempotency key after HTTP response loss.
Do not claim exactly-once delivery for a sink that cannot prove it. The debug
binary result is not an installed release qualification.

For H025, test the destination listener's request count, not only the HTTP
status at the origin. A refused redirect should leave the forbidden listener
at zero requests. Repeat with mixed public/private DNS answers, mapped
addresses, rebinding, ambient proxies, and each supported adapter and live
endpoint. Record approval capability and scope. The local adversarial suite is
`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-adversarial.ps1`.
It supplies regression evidence but does not replace the live matrix.

## Environment failures encountered during qualification

The earlier WSL SQLCipher build terminated with `Wsl/Service/E_UNEXPECTED`
before its migration test ran. Compiling part of SQLCipher is not a migration
pass. Docker and RAM allocation changes did not by themselves prove that gate;
the configured `.wslconfig` memory and swap values require a full WSL restart
and were not observed as applied while shared sidecars were running. Do not
restart the shared Docker/WSL services solely to satisfy this runbook. The
Windows Application Control policy blocked Cargo-generated build-script
executables with code 4551 in the release target; installing OpenSSL tooling
did not remove this policy block. A standard-library-only disposable relay
fixture compiled directly with `rustc` was used for the debug kill test. A
Docker SQLCipher control-plane release build got past the earlier WSL failure
and into Rust dependency compilation, but was stopped before image completion
when concurrent host builds left less than 1 GiB free RAM. Its cache remains.
Use a separate Cargo target directory for qualification to avoid disturbing
the active development build. Record environment failures separately from
product failures and rerun release and SQLCipher gates when the environment
permits.

After code changes, run affected crate tests, formatting and lint, the SPA
suite/lint if its UI changed, then the workspace and relevant release gates.
The 2026-09-30 evidence included 725 core tests, 1,180 server tests with five
ignored **before the final child-process regression changes**, and 537 SPA
tests with `--maxWorkers=2`; focused child, outbox, evidence, inference,
runner, sidecar, and endpoint tests passed later. A full server/workspace run
on the final source and release-installed qualification were not recorded.
Those suite counts predate subsequent source edits. The final isolated Windows
build and full adversarial script were stopped before completion; neither is
acceptance evidence for the final tree. Source files were also changing in a
second coding session during qualification. Retest from a settled tree before
changing any status to Complete.
Keep the ledger at Partial until the specified acceptance evidence exists.
