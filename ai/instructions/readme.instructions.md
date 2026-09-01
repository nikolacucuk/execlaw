# Coldfoot README Instruction Map

## Purpose

Use this file as the fast map for README discovery across the Coldfoot workspace.

It answers two questions quickly:

1. Which README should I open first for a task?
2. Which README files are maintained project docs vs third-party vendor docs?

## How To Use This Map

1. Start with the quick routing table.
2. Open the closest maintained README first.
3. Use third-party READMEs only for vendor IP behavior, not Coldfoot architecture ownership.
4. Treat RTL, `.core`, and `AGENTS.md` as source of truth if a README conflicts.

## Quick Routing By Task

| Task | Open First | Then Open |
|---|---|---|
| Repo overview, architecture status, setup | `README.md` | `AGENTS.md`, `docs/architecture.md` |
| Locate AI instruction/agent/junction behavior | `ai/README.md` | `.github/copilot-instructions.md`, `.github/instructions/dir_map.instructions.md` |
| FPGA build/program/bring-up on Nexys Video | `fpga/nexys_video/README.md` | `fpga/nexys_video/README_VIVADO_GUI.md`, `.github/instructions/fpga.instructions.md` |
| Vivado GUI-only workflow | `fpga/nexys_video/README_VIVADO_GUI.md` | `fpga/nexys_video/README.md` |
| ASIC GF180 template/flow on wafer.space lane | `asic/README.md` | `.github/instructions/gf180mcu.instuctions.md`, `.github/instructions/wafer.space.instructions.md` |
| Runtime CLI/service and graph loading | `sw/runtime/README.md` | `tools/dev/README.md` |
| Flow wrappers and command dispatch | `tools/dev/README.md` | `tools/flows/README.md` |
| Direct flow script usage (cocotb/formal/pnr/fpga) | `tools/flows/README.md` | `tools/dev/README.md` |
| Agent auto-fix loop behavior | `tools/agent/README.md` | `AGENTS.md` |
| Tile / gateway / mesh / neuron IP docs | `hw/ip/*/README.md` | `AGENTS.md`, module-local docs |
| Shared formal collateral in `hw/common` | `hw/common/formal/README.md` | `tools/flows/README.md` |
| Demo ECG training/inference pipeline | `demo/README.md` | `demo/tests/`, runtime docs |

## Maintained README Files (Project-Owned)

- `README.md`: repository-level architecture status, development setup, and maintained command entrypoints.
- `ai/README.md`: AI metadata linking workflow (`.github` junction/symlink setup) and generation behavior for Copilot instruction routing.
- `asic/README.md`: GF180/wafer.space template flow, Windows `ws` helper usage, and LibreLane targets.
- `demo/README.md`: ECG ANN-to-SNN experiment runbook and web inference usage.
- `fpga/nexys_video/README.md`: maintained Nexys Video FPGA lane, build/program commands, and flow status.
- `fpga/nexys_video/README_VIVADO_GUI.md`: interactive Vivado GUI project creation/open/build workflow.
- `hw/common/formal/README.md`: shared ready/valid and arbiter formal suite entrypoint.
- `hw/ip/host_gateway/README.md`: host boundary ownership, maintained targets, and host-gateway simulation entrypoints.
- `hw/ip/logical_neuron/README.md`: logical-neuron state/context/ucode banks and cocotb target entrypoints.
- `hw/ip/neural_mesh/README.md`: maintained mesh/router surface, scope boundaries, and mesh validation commands.
- `hw/ip/neuron_compute/README.md`: worker execution model and current verification anchors.
- `hw/ip/tile/README.md`: tile boundary role and top-level verification commands.
- `sw/runtime/README.md`: runtime crates, CLI/service usage, simulator URI flows, and graph/bundle behavior.
- `tools/agent/README.md`: `fix_loop.py` auto-fix workflow and safety boundaries.
- `tools/dev/README.md`: primary `tools/dev` runner/flow wrappers and environment contract.
- `tools/flows/README.md`: direct flow script usage for cocotb/formal/pnr/fpga.

## Third-Party README Files (Vendor Subtree)

No vendored third-party README files are tracked at present.  The previous
`fpga/nexys_video/lib/verilog-ethernet` submodule (alex forencich's Verilog
Ethernet stack) was retired with the unified-UART transport refactor and
removed from the repo (see commit 2baa173).

## Maintenance Checklist

When README files are added, removed, or moved:

1. Re-scan `README*.md` paths from repo root.
2. Update this file's maintained and third-party sections.
3. Keep one-line summaries short and task-oriented.
4. Keep this map index-only; do not duplicate full procedural instructions.