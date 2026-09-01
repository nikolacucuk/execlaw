# ColdFoot Main AI Instruction Router

This is the primary, minimal instruction file for this repository.

It is intentionally lightweight and should be treated as the first and only
instruction file loaded by default. Load additional instruction files only when
their trigger conditions are met.

## Copilot loading behavior

Copilot does not universally guarantee automatic loading of any arbitrary
`instructions.md` file in every product mode/session.

In this repository, use this file as the canonical router document. If your
Copilot setup supports persistent custom instructions, point that feature to
this file (or a thin wrapper that references this file) so routing behavior is
consistent.

## Always-on policy

1. Always load this file first.
2. Do not auto-load other instruction files by default.
3. Load only the smallest necessary set of additional instruction files.
4. Prefer one additional file per task unless the task is explicitly
   cross-domain.

## Conditional load matrix

Load these files only when the task matches the trigger.

### Architecture and repository map

- File: `ai/instructions/snn_asic.instuctions.md`
  - Load when: SoC architecture, hierarchy ownership, integration boundaries,
    NoC/tile/logical-neuron/worker roles, verification anchors.
  - Skip when: task is purely PDK/commercial/MPW logistics.

- File: `ai/instructions/tile_rtl.instructions.md`
  - Load when: implementing, reviewing, or optimizing SNN tile RTL,
    `tile_top`, logical-neuron banks, fanout/event queues, configurable
    neuron/synapse capacity, spike/flit limits, or area-focused wafer.space
    tile design.
  - Skip when: task is generic SoC architecture, pure MPW logistics, or not
    changing/reviewing tile-local RTL behavior.

- File: `ai/instructions/dir_map.instructions.md`
  - Load when: directory ownership, where-to-edit questions, path/source
    discovery, high-level repo navigation.
  - Skip when: task already scoped to a known file/module.

- File: `ai/instructions/readme.instructions.md`
  - Load when: README discovery, "which doc should I open", historical runbook
    lookup, and task-to-README routing questions.
  - Skip when: the task is already scoped to a specific module and known file.

### Technology and tapeout lane

- File: `ai/instructions/gf180mcu.instuctions.md`
  - Load when: GF180 PDK content, GF180 flow defaults, `asic/` template,
    LibreLane runs, GF180 sim/model path issues.
  - Skip when: task is SKY130-only.

- File: `ai/instructions/sky130.instructions.md`
  - Load when: SKY130 node/library selection, SKY130 path checks, SKY130
    backend runs.
  - Skip when: task is GF180-only.

- File: `ai/instructions/wafer.space.instructions.md`
  - Load when: wafer.space MPW constraints, slot/pricing/deadlines,
    tape-in/tape-out submission policy, wafer-space template specifics.
  - Skip when: generic SoC architecture or non-wafer-space tech work.

### FPGA development

- File: `ai/instructions/fpga.instructions.md`
  - Load when: Nexys Video FPGA builds, Vivado GUI/batch workflows, bitstream
    generation, UART validation, board bring-up, parameter tuning, driver
    troubleshooting.
  - Skip when: task is ASIC-only or generic hardware questions unrelated to
    Nexys Video.

## Multi-file loading rules

1. If task is MPW + GF180 technical execution:
   - Load `ai/instructions/wafer.space.instructions.md` and
     `ai/instructions/gf180mcu.instuctions.md`.
2. If task is architecture + implementation location:
   - Load `ai/instructions/snn_asic.instuctions.md` and
     `ai/instructions/dir_map.instructions.md`.
3. If task is tile RTL implementation or area optimization:
   - Load `ai/instructions/tile_rtl.instructions.md`.
   - Also load GF180/wafer.space instructions only when physical SRAM macro,
     LibreLane, area budget, or MPW constraints are directly involved.
4. If task is Nexys Video FPGA build/debug/validation:
   - Load `ai/instructions/fpga.instructions.md`.
   - Do not load GF180 or wafer.space files unless the task explicitly
     involves comparing FPGA and ASIC paths.
5. Never load both SKY130 and GF180 instruction files unless the user is
   explicitly comparing or migrating between them.
6. If uncertain, ask one short clarifying question before loading additional
   files.

## Agent routing (on-demand)

Load agent files only when the user asks for that workflow or the task clearly
matches that domain.

- `ai/agents/rtl_composer.agent.md`: RTL implementation/review/compliance
- `ai/agents/formal_writer.agent.md`: SystemVerilog formal generation/debug/validation
- `ai/agents/tt_flow_dbg.agent.md`: TinyTapeout/docker flow run-debug
- `ai/agents/ws_flow_dbg.agent.md`: wafer.space flow run-debug
- `ai/agents/ip_new.agent.md`: new IP environment/bootstrap
- `ai/agents/fpga_setup.agent.md`: Nexys Video FPGA build/program/validation
- `ai/agents/snn_gui.agent.md`: monitor UI on port 3000
- `ai/agents/ecg_gui.agent.md`: ECG web demo on port 8002

Do not load all agent files preemptively.

## Knowledge graph

- Rebuild Graphify with `make graph` after structural changes.
- Query `python tools/agent/query_graphify.py` before broad architecture scans.
- Use `python tools/agent/visualize_graphify.py --open` when a human wants an
  interactive graph view.
- The graph lives at `.graphify/graph.json` and is gitignored.
- The filesystem and `.core` manifests remain the source of truth.

Graphify-first policy for Copilot sessions:
- If `graphify-out/graph.json` or `.graphify/graph.json` exists, use it first
  for architecture/relationship questions to save tokens.
- Prefer `graphify query`, `graphify path`, or `graphify explain` over broad
  repository scans.
- Rebuild the graph only when explicitly requested or when graph files are
  missing/stale.

Note: To make the graph persistently available to Copilot Chat sessions (and
avoid expensive rebuilds), commit the produced graph file to the repo. This
workspace now allows committing `.graphify/graph.json` so assistants can load
the persisted graph automatically without re-running detection/extraction.

## Source-of-truth precedence

When content conflicts:

1. RTL and `.core` files
2. `AGENTS.md`
3. Focused instruction file from this router
4. Older READMEs/docs

## Minimal execution anchors

- Core flow wrapper: `tools/dev/flow`
- Backends: `tools/flows/*`
- Primary SoC core: `coldfoot:soc:coldfoot:0.1.0`
- Standard verification entrypoints are maintained in `AGENTS.md`

## Maintenance rules for this router

When updating this file:

1. Keep it short and routing-oriented.
2. Do not copy full technical details from child instruction files.
3. Update trigger rules when new instruction files are added or removed.
4. Keep agent list in sync with `ai/agents/*.agent.md`.
5. Prefer conservative loading to reduce unnecessary context.
