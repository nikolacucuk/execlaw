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
| H022 | Real local-model chat traversed Incomplete, Partial, VerifiedComplete, missing-artifact Blocked, and unconfirmed-delivery Incomplete. A fabricated attachment now reads Blocked and resubmission returns HTTP 400. | Real tasks through headless, editor, routine, and agent entries; plugin and research artifact producer links; deterministic verifier and delivery evidence for each relevant path. |
| H023 | A killed isolated Windows debug executable resumed held inference on the original run with one user event and one model reply. A child-inference process-kill regression reclaimed one model attempt and published one artifact. | Shared fixtures across streaming, in-process, runner, routine, and child paths; full process-kill matrix including paired tool events and no repeated completed effects. |
| H024 | Same-key retry returned the original run. A copied installed debug executable killed after a disposable subprocess sink accepted a send, before ack, restarted to Unknown with one attempt and one sink effect. | Release-installed binary and real transport/sink kill matrix, including a sink with supported idempotency or status lookup; review reconciliation decisions after restart. |
| H025 | Exact-model TrueNAS inference passed through a scoped approval; removing it returned HTTP 503, and another capability's grant did not authorize it. A sidecar redirect test sent zero requests to the destination listener. | Live supported endpoints across public web, research, automation, plugin HTTP, MCP, OAuth, and sidecar paths, with DNS rebinding, mixed answers, redirects, proxies, and zero prohibited requests. |

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
It covers one cut, not the entire matrix. A runner-enabled isolated server
must use a separate Docker daemon/context until `boot_orphan_sweep` is scoped
to its own installation: the current sweep can remove another installation's
runner volumes on a shared daemon. Verify PID, command line, database, and
Docker context before any process kill.

When the disposable port-3032 server was stopped, its log showed repeated
startup recovery attempts for `turn:qa-real-h022-review-1790791531633:1`.
Inference first timed out, then the run exceeded its wall-clock budget, yet
startup recovery kept returning HTTP 500 and retrying. This is an observed
unresolved H023 case. A future fix should make the exhausted run terminal or
Controller-reviewable without a hot retry loop, and add a restart regression
using the same durable run. Preserve the disposable database for diagnosis;
do not copy it over an operator database.

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
Windows Application Control policy also blocked a fresh Cargo build-script
process with code 4551; a standard-library-only disposable relay fixture
compiled directly with `rustc` was used for the debug process-kill test.
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
`cargo check -p execlaw-server -p execlaw-runner` passed on the final source
with three server dead-code warnings; a workspace format check still found
differences in `server/src/lib.rs` and `server/src/runner_spawn.rs` in the
shared working tree. The scoped documentation whitespace check passed.
Keep the ledger at Partial until the specified acceptance evidence exists.
