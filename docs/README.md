# Documentation and implementation tracking

The accepted plan is to implement **all 130 enhancements, H001-H130**.
Start with the [implementation ledger](implementation-plan.md) for ownership,
status, sequencing, and evidence. The [harness roadmap](llm-harness-roadmap.md)
owns requirements and acceptance criteria, including the F01-F19 review
findings. The [immediate queue](remaining-improvements-todo.md) identifies the
first bounded delivery slices. Planned scope must not be described as already
shipped, and historical checklists do not override the ledger.

## Planning and architecture

| Document | Role in the 130-item plan |
|---|---|
| [Implementation plan](implementation-plan.md) | Complete H001-H130 status/owner/evidence ledger and F01-F19 closure mapping |
| [Harness roadmap](llm-harness-roadmap.md) | Stable requirement IDs, priorities, dependencies, acceptance criteria, and findings |
| [Nexus visual extension](llm-harness-roadmap.md#nexus-visual-roadmap) | NX01-NX30 proposals, settings/off contract, synthetic/live screenshots, and NXF01-NXF11 findings; tracked separately from H001-H130 |
| [Immediate queue](remaining-improvements-todo.md) | Release blockers and unresolved validation; not the whole backlog |
| [Architecture](architecture.md) | System boundaries and design invariants; historical milestones are labelled |
| [Agent model](agent-model.md) | Turn execution, context, trust, memory, and delegation contracts |
| [Runner design](runner-design.md) | Runner lifecycle, protocol, execution isolation, and recovery |
| [Sidecar supervision](sidecar-supervisor-design.md) | Managed local services, runtime ownership, and resource controls |
| [Plugin author reference](plugins.md) | Manifest/runtime/tool/transport/UI contracts and planned qualification |
| [Memory roadmap](memory-roadmap.md) | Detailed memory work breakdown, subordinate to H-item completion status |
| [Automation design](automations.md) | Graph execution, revisions, scheduling, and safe simulation |
| [Voice follow-ups](voice-followups.md) | Local speech pipeline and planned interruption/grounding work |
| [Operator decision rubric](operator-decision-rubric.md) | Placement of new functionality in plugins, MCP, or host core |
| [Improvement strategy](execlaw_impr_doc.md) | Historical research and rationale; old TODOs are superseded by the ledger |

## Verification and security

| Document | Evidence it helps produce |
|---|---|
| [Testing](testing.md) | Relevant checks, execution-path distinctions, and qualification requirements |
| [Adversarial evaluations](adversarial-evaluations.md) | Deterministic enforcement checks and the expanded security test program |
| [H022-H025 qualification](h022-h025-qualification.md) | Local-model recovery, completion evidence, process-kill, endpoint-policy checks, and remaining acceptance gates |
| [Security](security.md) | Threat model, current limitations, and open findings |
| [Key rotation drill](key-rotation-drill.md) | SQLCipher, backup/restore, key continuity, and incident recovery |
| [Skill evaluation](skill-evaluation.md) | Current evaluator scope and planned executable held-out tests |
| [June security hardening](security-hardening-2026-06.md) | Dated remediation record; not proof that F01-F19 are closed |

## Deployment and operator guides

| Document | Scope |
|---|---|
| [Desktop installations](desktop-installations.md) | Cross-platform packaging and pending artifact qualification |
| [Ollama](ollama.md) | Native local model serving and related qualification work |
| [Setup walkthroughs](setup-walkthroughs.md) | Transport/integration configuration |
| [macOS setup](setup-mac.md) | Platform-specific installation context |
| [TrueNAS Docker](truenas-docker.md) | Deployment workflow and local inference connectivity |
| [TrueNAS NVIDIA/Ollama setup](truenas-docker-nvidia-ollama-setup.md) | Hardware-specific deployment walkthrough |
| [TrueNAS deployment reference](TrueNAS_deploy_doc.md) | Additional deployment context; use current security/release gates |
| [Workspace Graphify/Obsidian setup](copilot-graphify-obsidian-workspace-setup.md) | Developer knowledge workflow |
| [Superpowers integration](superpowers-integration.md) | Skill integration context |

The root [README](../README.md), [web README](../web/README.md),
[plugin README](../plugins/README.md), crate READMEs, and
[macOS](../desktop-macos/README.md), [Windows](../desktop-windows/README.md),
and [Linux](../desktop-linux/README.md) READMEs link their relevant H items.
Asset READMEs describe screenshots, icons, or bundled binary layout; they are
not independent delivery trackers.

## Historical and scenario references

[Hermes porting](hermes-porting-todo.md),
[thread deletion investigation](chat_thread_del_bug.md), and
[camper agent handling](camper_wha_agent_handling.md) retain their original
context. Historical completion marks describe the recorded scope and date;
they do not qualify later enhancements or override current review findings.
The [screenshot guide](screenshots/README.md) describes documentation assets,
not the implementation status of pictured features.

## Keeping documentation consistent

For each implementation slice, update the H ledger row and its evidence,
reconcile the roadmap checkbox, and update the subsystem docs affected by
actual behavior. Close related F findings only with their own regression
evidence. Keep historical reports dated rather than presenting them as fresh
test results. New APIs, permission changes, migrations, model/platform support,
and operational recovery steps need corresponding reference updates.

Do not copy the 130-item status table into other documents. Link stable
`enhancement-NNN` and `finding-fNN` anchors instead. An implementation does not
become release-qualified until the [plan's evidence gates](implementation-plan.md#qualification-and-evidence-required-for-completion)
are satisfied. This documentation update itself implements no runtime feature.
