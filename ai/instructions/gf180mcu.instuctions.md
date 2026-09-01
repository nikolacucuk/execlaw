# ASIC Technology Instructions: GF180MCU (ColdFoot_SoC)

## Scope

This file is GF180MCU-only.

This is an ASIC technology instructions file for GF180MCU process usage in this workspace.

- Do not document other PDKs here.
- Add other PDK guidance in separate instruction files.

## Purpose

Use this file to:

- understand GF180MCU PDK structure and documentation coverage,
- map where GF180MCU is used by local ColdFoot ASIC flows and RTL,
- run or debug GF180MCU-backed LibreLane and simulation paths,
- avoid wrong PDK-root or missing-submodule assumptions.

## Local GF180MCU PDK Repo Summary

Observed top-level structure:

- docs/: documentation entrypoint and topic trees
- libraries/: foundry library packages (via submodule paths)
- macros/: macro/IP packages (via submodule paths)
- third_party/: external dependencies, including open-source-pdks submodule path
- README.rst, LICENSE, .gitmodules, Makefile

Important findings from local repo state:

- The local checkout documents GF180MCU as a 0.18um 3.3V/(5V)6V MCU process open PDK.
- Current status is documented as experimental preview/alpha (not production signoff-ready by default).
- Several documentation entries resolve through submodule-backed paths.
- In this local downloaded copy, key submodule folders are present but empty:
  - libraries/gf180mcu_fd_sc_mcu7t5v0/latest
  - libraries/gf180mcu_fd_sc_mcu9t5v0/latest
  - libraries/gf180mcu_fd_io/latest
  - libraries/gf180mcu_fd_pr/latest
  - libraries/gf180mcu_fd_bd_sram/latest
  - macros/gf180mcu_fd_ip_sram/latest
  - third_party/open-source-pdks
- Result: some docs are pointer files to external submodule docs, not expanded content in this local snapshot.

## Important PDK Information (Do Not Omit)

From README and docs tree in the local GF180 repo:

- Technology/process framing:
  - GF180MCU is documented as a 0.18um MCU process with 3.3V and 6V-class support.
  - Documentation banner references 3.3V/(5V)6V MCU flows and physical-rule manuals.

- Release maturity and risk:
  - Public open-source release is described as experimental preview/alpha.
  - Intended for test chips and initial verification, not assumed production-qualified.

- Documentation organization:
  - Digital: standard-cell library sections for gf180mcu_fd_sc_mcu7t5v0 and gf180mcu_fd_sc_mcu9t5v0.
  - Analog/custom: spice specs, layout specs, and model-parameter guides.
  - IPs: IO and SRAM sections.
  - Physical verification: design manual and design-rule content.
  - open-source-pdks integration hook is explicitly present.

- Model documentation scope:
  - LV model reference guide is present for 0.18um 3.3V/6V HV MCU process.
  - HV model reference guide is present for 0.18um 10V HV MCU process.

- Physical verification/design-manual coverage:
  - Includes topological definitions and layer/mask truth tables.
  - Includes DFM guidelines and tapeout checklist.
  - Includes geometry rules across front-end/back-end layers.
  - Includes antenna rules.
  - Includes bond-pad, CUP, and solder-bumping guidance.
  - Includes analog-device rules (resistors, MIM options, LDMOS/HV details, eFuse).
  - Includes SRAM-core related rules.
  - Includes scribe-line/guard-ring rules.
  - Includes dummy-fill guidance.
  - Includes reliability guidance: EM, latch-up, ESD, stress relief, slotting.
  - Includes appendix for modeled/LVS device lists and explicit rules-not-coded section.

## Library and Macro Packaging Notes

The local repo defines these package lanes in .gitmodules:

- libraries/gf180mcu_fd_pr/latest
- libraries/gf180mcu_fd_sc_mcu7t5v0/latest
- libraries/gf180mcu_fd_sc_mcu9t5v0/latest
- libraries/gf180mcu_fd_bd_sram/latest
- libraries/gf180mcu_fd_io/latest
- macros/gf180mcu_fd_ip_sram/latest
- third_party/open-source-pdks

Practical implication:

- If those submodules are not populated, PDK assets may appear present by path but be unusable by tools.
- Validate actual files under libs.ref/libs.tech before running implementation or simulation.

## Local ColdFoot GF180MCU Usage Map

### Flow defaults and maintained backend path

- tools/flows/tool_flow.py:
  - SoC PNR defaults map to gf180mcuD + gf180mcu_fd_sc_mcu7t5v0.
- tools/dev/flow.py:
  - soc_coldfoot and soc_rv_sw_async defaults map to gf180mcuD + gf180mcu_fd_sc_mcu7t5v0.
  - pnr target soc_rv_sw_async is routed through the maintained soc_coldfoot LibreLane config.
- asic/librelane/config.yaml (+ config.dev.yaml/config.medium.yaml):
  - maintained GF180 template tuning and PDK/SCL assumptions live in the LibreLane YAML set.

### ASIC template and local project flow usage

- asic/Makefile:
  - defaults PDK_ROOT to asic/gf180mcu,
  - defaults PDK to gf180mcuD,
  - provides clone-pdk target (wafer-space/gf180mcu fork),
  - runs LibreLane with --manual-pdk and explicit --pdk/--pdk-root.
- asic/README.md:
  - documents gf180mcu project template and gf180mcuD flow assumptions.

### RTL and macro usage tightly coupled to GF180MCU cells

- asic/src/chip_top.sv instantiates GF180 IO/pad cells and wafer.space GF180 macros, including:
  - gf180mcu_fd_io__in_s
  - gf180mcu_fd_io__in_c
  - gf180mcu_fd_io__bi_24t
  - gf180mcu_fd_io__asig_5p0
  - gf180mcu_ws_io__dvdd / gf180mcu_ws_io__dvss
  - gf180mcu_ws_ip__id / gf180mcu_ws_ip__logo
- asic/src/chip_core_tile.sv instantiates GF180 SRAM macro:
  - gf180mcu_fd_ip_sram__sram512x8m8wm1

### LibreLane macro hookups and timing views

- asic/librelane/config.yaml references GF180 macro views for:
  - local custom macros gf180mcu_ws_ip__id and gf180mcu_ws_ip__logo,
  - PDK SRAM macro gf180mcu_fd_ip_sram__sram512x8m8wm1
- Includes PDK library references for GDS/LEF/verilog/lib timing corners.

### Simulation usage

- asic/cocotb/chip_top_tb.py expects GF180 files under:
  - libs.ref/gf180mcu_fd_io/verilog
  - libs.ref/gf180mcu_fd_ip_sram/verilog
  - selected SCL verilog path (for GL runs)

## Local Installation Reality Check in This Workspace

Current workspace-local .pdks inventory indicates SKY130 installation but no detected GF180MCU installation under .pdks.

Implication:

- For SoC pnr_coldfoot runs that assume project-local .pdks, verify GF180 content exists before launch.
- For asic/ template runs, ensure asic/gf180mcu (or chosen PDK_ROOT) is populated and consistent with selected PDK value.

## GF180MCU Command Patterns

### Maintained SoC PNR path (repo flow wrapper)

```sh
tools/dev/flow pnr soc_coldfoot --pdk-root /foss/designs/coldfoot_soc/.pdks
```

### ASIC template path (standalone under asic/)

```sh
cd asic
make clone-pdk
make librelane
```

### ASIC simulation path (standalone under asic/)

```sh
cd asic
make sim
make sim-gl
```

## Validation Checklist

1. Confirm selected PDK (typically gf180mcuD) exists under PDK_ROOT.
2. Confirm libs.ref and libs.tech content is populated (not empty submodule stubs).
3. Confirm selected SCL exists (typically gf180mcu_fd_sc_mcu7t5v0 for maintained SoC flow).
4. Confirm GF180 IO and SRAM macro models resolve in simulation and implementation logs.
5. Confirm no accidental cross-PDK mix during runs (for example sky130 defaults in unrelated targets).

## Source Anchors

- tools/flows/tool_flow.py
- tools/dev/flow.py
- asic/librelane/config.yaml
- asic/README.md
- asic/Makefile
- asic/librelane/config.yaml
- asic/src/chip_top.sv
- asic/src/chip_core_tile.sv

## Update Policy

When updating this file:

1. Re-check local gf180mcu-pdk-main docs and submodule population state.
2. Re-check maintained SoC flow defaults in tools/flows/tool_flow.py and tools/dev/flow.py.
3. Re-check asic/librelane/config*.yaml PDK/SCL scopes.
4. Keep this file strictly GF180MCU-scoped.