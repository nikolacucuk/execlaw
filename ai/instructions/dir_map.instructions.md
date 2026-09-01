# ColdFoot_SoC Directory Map Instructions

## Purpose

Use this file as the maintained directory map for the ColdFoot_SoC workspace.

- Workspace name: ColdFoot_SoC
- Chip name: Cold Foot
- Domain: Spiking Neural Network (SNN) ASIC/SoC with FPGA and runtime tooling

This map is intentionally relationship-first: it explains what each directory is for and how directories depend on each other.

## Mapping Rules

When updating this map:

1. Treat FuseSoC core descriptors and active README files as source-of-truth.
2. Prefer describing maintained paths over legacy/retired paths.
3. Separate source directories from generated/build artifact directories.
4. Keep architecture language SNN-correct: neuron-centric firing, fan-out connectivity, event-driven flow.
5. When a path is a junction/symlink, document both the visible path and the target.

## Top-Level Structure (Curated)

```text
coldfoot_soc/
   .archive/            Local archive/snapshot area (non-source)
   .claude/             Claude/Codex local agent settings and hooks
   .githooks/           Repository-managed Git hooks (Graphify refresh)
   .github/             Copilot-facing mirror/junction layer for ai/ metadata
  ai/                 AI guidance, agents, instructions, and workflow metadata
  asic/               Wafer.space/GF180MCU ASIC implementation template and flow
  demo/               ECG/SNN model training, conversion, and web inference demo
   docker/             Dockerfiles and container lane definitions
  docs/               Cross-cutting project design/flow documentation
  edalize/            Edalize helper backend(s)
  fpga/               Nexys Video FPGA tops, constraints, and scripts
   hw/                 RTL source-of-truth: common blocks + maintained IPs
  scripts/            Repo utility scripts (host-side helpers)
  sw/                 Runtime service/CLI and monitor UI
  tools/              Developer entrypoints and flow orchestration
  README.md           Root architecture + setup + command reference
  AGENTS.md           Agent runbook and repository contracts
  fusesoc.conf        FuseSoC library configuration
   graphify.toml       Graphify configuration
   makefile            Repo task entrypoints
   tt.ps1 / tt.cmd     TinyTapeout helper wrappers
```

## Hardware Source Split

### hw/common

Shared hardware building blocks used by multiple IPs.

- `packages/tile_pkg.sv` and `packages/tile_flit_types.vh`: shared protocol constants, packet semantics, and tile-local payload structs
- `mem/coldfoot_mem_*`: backend-neutral memory wrappers (preferred over ad hoc local RAM helpers)
- `rv_lib/`, `arb/`, and `struct/`: reusable utility blocks and interfaces

### hw/ip

Maintained reusable IP blocks.

- `host_gateway/`: host protocol boundary, mesh ingress/egress adaptation, telemetry/trace/control CSR handling, and bundle-loader ownership
- `neural_mesh/`: maintained NoC/router fabric and flit/packet transport primitives
- `tile/`: maintained tile boundary (`tile_top`) and tile-local scheduling/event flow
- `logical_neuron/`: tile-local persistent logical-neuron control/context/ucode banks
- `neuron_compute/`: lightweight worker execution engines (`neuron_compute_core.sv`, `neuron_exec.sv`)

### SoC Integration Ownership

There is no maintained `hw/SoC/` directory in this workspace layout.

Current integration ownership is split by lane:

- `asic/src/`: ASIC top wrappers (`chip_top.sv`, `chip_core_tile.sv`) and tapeout-oriented integration
- `fpga/nexys_video/rtl/`: FPGA board-top integration/wrappers
- `hw/ip/neural_mesh/` + `hw/ip/host_gateway/`: maintained mesh/gateway integration boundary

If older docs mention `hw/SoC/*`, treat those references as historical until they are migrated.

## Software and Runtime Split

### sw/runtime

Rust-first runtime stack and language bindings.

- CLI package for board/runtime commands
- Service package exposing HTTP/WebSocket API for monitor and tools
- Shared runtime core for graph/program/bundle/telemetry models
- Python binding package for integration use-cases

### sw/monitor

Browser UI (Node/Vite) that visualizes runtime graph state and telemetry from `sw/runtime` service.

## FPGA and ASIC Split

### fpga/nexys_video

Vivado-centric FPGA flow for the same Coldfoot architecture.

- Board tops and wrappers (`rtl/`)
- constraints (`constraints/`)
- build scripts (`scripts/`)
- README files for batch flow and GUI workflow

### asic

Wafer.space GF180MCU implementation template and standalone flow collateral.

- `src/chip_top.sv`, `src/chip_core_tile.sv`: ASIC top-level/padframe side
- `librelane/`: ASIC physical-flow configs and slots

This tree is parallel to `hw/`-centric SoC development, not a duplicate of every source path.

### docker

Container definitions for maintained tool lanes.

- `wafer-space-gf180ns.Dockerfile`: wafer.space GF180 container build description

## Tools and Flow Ownership

### tools/dev

User-facing command entrypoints and wrappers.

- `flow` / `flow.ps1`: main dispatcher for lint/sim/formal/pnr/fpga commands
- `run` / `run.ps1`: native vs docker execution wrapper
- `doctor` / `doctor.ps1`: host/container toolchain checks
- `coldfoot*` wrappers: runtime service/CLI wrappers (Windows-friendly)

### tools/flows

Backend flow scripts called by `tools/dev/flow*`.

- `tool_flow.py`: formal and ASIC-flow orchestration
- `cocotb_flow.py`: cocotb simulation flow
- `fpga_program.py`: TCL-generating wrapper invoked by the FuseSoC `program`
  and `flash` script hooks on `coldfoot:fpga:nexys_video`.  The full Vivado
  FPGA lifecycle (lint / synth / bitstream / program / flash) is driven by
  FuseSoC targets — see `ai/instructions/fpga.instructions.md`.

### tools/agent

Automated fix loop (`fix_loop.py`) that runs checks and applies patch proposals in bounded iterations.

## AI Guidance Split

### ai

- `agents/*.agent.md`: specialized agent instructions
- `instructions.md`: top-level AI folder guidance
- `instructions/`: maintained instruction-file directory for architecture/flow lanes
- `skills/`, `workflows/`: extension points (currently empty in this workspace)

### .github (Junction Layer)

In this workspace, several `.github` directories are junctions to `ai/` paths:

- `.github/agents` -> `ai/agents`
- `.github/instructions` -> `ai/instructions`
- `.github/skills` -> `ai/skills`
- `.github/workflows` -> `ai/workflows`

Treat `ai/` as canonical content ownership.

## Documentation and Demo Areas

### docs

Project-wide flow/architecture planning docs.

- `fusesoc-migration-plan.md`: Makefile -> FuseSoC migration plan
- `loader-protocol-audit.md`: loader path and protocol behavior audit
- `fpga-fullcore-synth-rerun-checklist.md`: maintained full-core FPGA rerun checklist

### demo

ECG/SNN software experimentation pipeline.

- ANN training + SNN conversion/fine-tuning scripts
- web inference UI (`infer_spikingjelly_web.py`, `web/`)
- test coverage in `demo/tests/`

This area supports algorithm/demo workflows and can be used with runtime/FPGA paths, but it is not the RTL source-of-truth.

## Relationship Graph (Conceptual)

```text
sw/monitor
   -> sw/runtime service
      -> runtime protocol/messages
         -> hw/ip/host_gateway (host ingress/egress, CSR, loader)
            -> hw/ip/neural_mesh (fabric transport)
               -> hw/ip/tile (tile boundary + scheduling)
                  -> hw/ip/logical_neuron (state/ucode banks)
                  -> hw/ip/neuron_compute (worker execution)
                     -> hw/common (message defs, interfaces, memory wrappers)

tools/dev -> tools/flows -> FuseSoC cores in hw/* -> sim/formal/pnr/fpga targets
fpga/nexys_video wraps maintained hw/ip design intent for board bring-up
asic/ provides wafer-space/librelane implementation path for chip tapeout-oriented flows
```

## Maintained README Index

- `README.md`
- `tools/dev/README.md`
- `tools/flows/README.md`
- `tools/agent/README.md`
- `hw/ip/tile/README.md`
- `hw/ip/host_gateway/README.md`
- `hw/ip/neural_mesh/README.md`
- `hw/ip/logical_neuron/README.md`
- `hw/ip/neuron_compute/README.md`
- `fpga/nexys_video/README.md`
- `fpga/nexys_video/README_VIVADO_GUI.md`
- `sw/runtime/README.md`
- `demo/README.md`
- `asic/README.md`

## Update Checklist

Before accepting directory map changes:

1. Verify new/removed directories with actual workspace listing.
2. Confirm README references still exist and describe current behavior.
3. Confirm FuseSoC core names/locations still match `fusesoc.conf` and `*.core` files.
4. Confirm maintained architecture language still reflects packed tile + logical neuron + worker split.
5. Keep generated artifacts (for example `runs/`, `sim_build/`, `node_modules/`, `target/`) marked as non-source directories.
