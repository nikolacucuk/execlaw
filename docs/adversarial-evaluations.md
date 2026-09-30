# Offline adversarial evaluations

## Committed enforcement work

The [implementation plan](implementation-plan.md) tracks H001-H130; this page
owns the adversarial-evidence view, not a separate backlog. Existing fixtures
under [H017](llm-harness-roadmap.md#enhancement-017) remain a baseline.
They do not close the [open roadmap findings](llm-harness-roadmap.md#review-findings-and-unresolved-verification).
F01/F19 require complete-router authorization tests; F02 requires failed-upgrade
and traversal tests; F03 requires proving no prohibited connection occurred;
F18 requires proving simulation cannot reach live effects. A rejection returned
after a side effect is not successful containment.

Planned expansion follows [H025 egress](llm-harness-roadmap.md#enhancement-025),
[H026 authorization](llm-harness-roadmap.md#enhancement-026),
[H052 dispatch-time authority](llm-harness-roadmap.md#enhancement-052),
[H086 instruction precedence](llm-harness-roadmap.md#enhancement-086),
[H104 webhook replay/rotation](llm-harness-roadmap.md#enhancement-104),
[H107 model checking](llm-harness-roadmap.md#enhancement-107),
[H108 parser/protocol fuzzing](llm-harness-roadmap.md#enhancement-108),
[H109 mutation testing](llm-harness-roadmap.md#enhancement-109), and
[H110 denied-network qualification](llm-harness-roadmap.md#enhancement-110).
Use disposable databases, fake credentials, and controlled mock sinks for
effectful cases. [H126](llm-harness-roadmap.md#enhancement-126) must establish a
separate simulation dispatcher; today's automation Test run is not one.
Record executed results in the tracker without treating this planned coverage
as already passing.

execlaw's deterministic adversarial suite runs without a model, network access,
or secrets. It exercises the same policy, runner, HTTP-tool, and conversation
search code used by the application. Run it with:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\test-adversarial.ps1
```

The script runs the focused fixtures so unrelated tests in those large crates
do not prevent the security suite from reporting its own status. The broader
package suites can also be run with `cargo test -p execlaw-policy -p
execlaw-core -p execlaw-runner-local -p execlaw-server`.

The fixtures cover these trust boundaries:

| Attack class | Regression coverage |
|---|---|
| Prompt injection and delimiter smuggling | `policy::spotlighting` wrapping tests; trust planner/executor tests in `server::chats` prove the planner has no tool catalog and the executor uses the reviewed handoff. |
| Malformed tool calls and arguments | `runner-local::turn::malformed_tool_arguments_are_rejected_without_dispatch` and model-family schema fixtures; invalid arguments never reach dispatch. |
| Unicode spoofing | `policy::input_guard` strips zero-width and bidi controls and folds the covered Cyrillic, Greek, and Armenian lookalikes. |
| SSRF | `server::tool_apis_http` has scheme and selected literal-address rejection fixtures. F03 remains open: DNS, mapped-address, and redirect checks do not enforce the full connection boundary. |
| Cross-conversation/trust memory leakage | `server::chats::nexus::search_is_conversation_scoped_and_fails_closed_on_hmac_tampering`; trust-policy tests also check the planner/executor split for untrusted principals. |

These tests assert deterministic enforcement properties; they do not use an LLM
judge. New attack cases should be added next to the enforcement code and included
in the command above. For optional model-quality evaluation, use the existing
`eval-harness` only with a local or explicitly approved endpoint, record the
model identifier and fixture revision, and keep deterministic enforcement tests
as the CI gate. No cloud judge is supported.
