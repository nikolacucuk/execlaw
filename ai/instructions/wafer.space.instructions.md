# MPW Program Instructions: wafer.space (ColdFoot_SoC RevA)

## Scope

This file is wafer.space MPW program only.

This is a shuttle and execution instructions file for wafer.space based
GF180MCU tape-in and tape-out work in this workspace.

- Do not duplicate generic GF180 process guidance here.
- Keep process technology truth in `.github/instructions/gf180mcu.instuctions.md`.
- Keep this file focused on MPW business/operational constraints,
	submission requirements, slot configuration, and template usage.

## Technology Pointer (Authoritative)

The wafer.space MPW described in this file is implemented on GF180MCU.

Technology source of truth for this workspace:

- `.github/instructions/gf180mcu.instuctions.md`

Use that file for:

- PDK structure and maturity status,
- local ColdFoot flow mappings (`tools/flows`, `tools/dev/flow`, `hw/ip` + `asic`),
- GF180 library/macro availability checks,
- cross-checking PDK root and SCL assumptions.

## Purpose

Use this file to:

- understand wafer.space MPW requirements that govern RevA tape-in/tape-out,
- map slot, pad, packaging, timeline, and submission constraints to ColdFoot,
- avoid invalid assumptions about responsibilities (designer vs wafer.space),
- align local template usage with wafer.space deliverable expectations,
- capture practical runbook steps for sign-off and submission.

## wafer.space Program Snapshot

Primary service framing from wafer.space pages:

- Budget pooled multi-project silicon manufacturing.
- Process node: GlobalFoundries GF180MCU (180nm mixed-signal lane).
- Standard quantity: 1000 dies per purchased slot.
- wafer.space handles backend logistics after submission:
	fabrication, dicing, and delivery/shipping.

Additional campaign and operations details from CNX and wafer.space pages:

- wafer.space accepts designs submitted as GDSII and performs manufacturability
	checks before fabrication handoff.
- Designs may be open or closed source according to CNX campaign summary.
- Tooling can be open-source or commercial as long as output/sign-off is valid.
- wafer.space campaign links route through Crowd Supply / buy.wafer.space.
- Fabrication/packaging logistics are presented as Singapore-centered operations.
- Full undiced wafer option is positioned for advanced research/custom dicing use.
- CNX notes future ecosystem intent around direct PCBA/bonding paths with
	third-party PCB vendors; treat these as directional, not guaranteed defaults.
- Historical CNX wording used "3.88 x 5.07 mm" for full-slot geometry, which
	aligns with usable silicon dimensions rather than full die dimensions.

## Company-Implied Responsibility Split (Critical)

From wafer.space technology/process pages, the implied contract is:

### Customer responsibilities (you)

1. Reserve slot.
2. Design chip using GF180MCU PDK and selected tool flow.
3. Complete sign-off checks (DRC/LVS/ERC) and design-guideline compliance.
4. Submit tape-in files according to wafer.space checklist (GDS-centric).

### wafer.space responsibilities (after submit)

5. Fabrication in pooled MPW run.
6. Wafer dicing and post-fab handling.
7. Delivery of bare dies and optional packaged outputs.

Practical implication for ColdFoot RevA:

- Tape-in quality and sign-off evidence are a design-team obligation.
- Do not assume wafer.space will fix customer-side DRC/LVS/ERC defects.

## Timeline and Campaign Constraints

Time-sensitive data captured from wafer.space Run 2 pages:

- Campaign Opens: 1 March 2026
- Early Bird Deadline: 30 April 2026 @ 11:59 PM AoE
- Purchase and Submission Deadline: 30 June 2026 @ 11:59 PM AoE
- Parts shipped target: Early Q4 2026

Historical context from CNX Run 1 article (for reference, not current policy):

- Run 1 campaign close cited as 28 Nov 2025.
- Run 1 submission deadline cited as 3 Dec 2025.
- Run 1 shipment expectation cited as March 2026.

Use current wafer.space pages as authoritative for active run dates.

## Pricing and Commercial Model

### Slot purchase (Run 2 page data)

- Full slot (1x1):
	- Early Bird: USD 7000 (USD 7.00/die)
	- Standard: USD 7500 (USD 7.50/die)
- Half-width slot (0.5x1):
	- Early Bird: USD 4000 (USD 4.00/die)
	- Standard: USD 4500 (USD 4.50/die)
- Half-height slot (1x0.5):
	- Early Bird: USD 4000 (USD 4.00/die)
	- Standard: USD 4500 (USD 4.50/die)

**Note: Full slot (1x1) is used by ColdFoot_SoC.

### Add-ons

- Chip on Board (COB) packaging add-on:
	- USD 1500 total (USD 1.50/die)
	- Requires default pad ring
	- Wire-bonded small PCBs for immediate bring-up/testing
- Full undiced wafer add-on:
	- USD 2000
	- 200mm wafer containing all designs in that run
	- Requires slot purchase

## Slot Geometry, Area, and I/O Constraints

Important: public sources may report either die area or usable area.

- Die area includes seal ring boundary.
- Usable silicon is inside seal ring.
- Core area (in default template) is inside default IO ring.

### wafer.space pricing/technology numbers (Run 2 pages)

- 1x1:
	- Die size: 3.93 x 5.12 mm (20.12 mm2)
	- Usable silicon: 19.65 mm2
	- Default IO pads: 56
	- Max IO pads with custom pad ring: up to 168
	- Core area with default pad ring: 12.92 mm2
- 0.5x1:
	- Die size: 1.94 x 5.12 mm (9.93 mm2)
	- Usable silicon: 9.57 mm2
	- Default IO pads: 56
	- Max IO pads with custom pad ring: up to 122
	- Core area with default pad ring: 4.46 mm2
- 1x0.5:
	- Die size: 3.93 x 2.53 mm (9.94 mm2)
	- Usable silicon: 9.61 mm2
	- Default IO pads: 56
	- Max IO pads with custom pad ring: up to 108
	- Core area with default pad ring: 5.02 mm2

### Slot JSON/template numbers (mithro docs)

The slot docs and JSON include four template slot families:

- 1x1 (full)
- 0.5x1 (half width)
- 1x0.5 (half height)
- 0.5x0.5 (quarter)

Quarter-slot note:

- 0.5x0.5 appears in template/docs with detailed geometry and pad options,
	but wafer.space purchase pages emphasize 1x1 and half-slot offerings.
- Validate active commercial availability before planning RevA on quarter slot.

## Slot Configuration System (mithro project-template docs)

Configuration naming pattern:

- `slot_density_edges`
- Example: `0p5x0p5_max_all`

Density modes:

- `def`: default mixed bidir/input/analog pad profile
- `max`: maximum pad density (mostly bidir)
- `spc`: 1x1 spacing compatibility mode
- `num`: 1x1 pad-count compatibility mode

Edge modes:

- `all`: all edges
- `top`: north only
- `lft`: west only
- `hor`: north + south
- `ver`: east + west
- `nwc`: north + west
- `sec`: south + east

Seal ring convention documented in slot guides:

- Approximately 26um per edge subtracted from die to usable area.

## Process and Design Capability Notes

Across wafer.space and CNX references:

- GF180MCU mixed-signal process at 180nm.
- 5 metal layers available.
- MIM and MOS capacitor support.
- Poly and high-resistor options.

Run 1 empirical data references published by wafer.space:

- 29 open-source designs listed on one reticle showcase.
- Reported practical digital logic densities commonly in ~3k to 14k cells/mm2,
	with top examples higher.
- Reported real designs often land around 10% to 44% of theoretical maxima,
	depending on routing, infrastructure, PDN, and clocking overheads.

Practical implication:

- Do not budget silicon capacity from theoretical density alone.
- Size using realistic utilization envelopes and congestion margins.

## Capacity Reference (wafer.space technology page)

Published theoretical logic capacities (default pad ring context):

- 1x1 slot:
	- FF_1: about 185,014
	- BUF_4: about 393,156
	- AND3_2: about 508,790
- 0.5x1 slot:
	- FF_1: about 63,867
	- BUF_4: about 135,718
	- AND3_2: about 175,635
- 1x0.5 slot:
	- FF_1: about 71,886
	- BUF_4: about 152,759
	- AND3_2: about 197,688

Published SRAM capacity estimates (technology page):

- GF 5V sram512x8:
	- 1x1: 40 KB (80 blocks)
	- 0.5x1: 20 KB (40 blocks)
	- 1x0.5: 20 KB (40 blocks)
- GF 5V sram256x8:
	- 1x1: 28 KB (112 blocks)
	- 0.5x1: 14 KB (56 blocks)
	- 1x0.5: 14 KB (56 blocks)
- 3.3V sram1024x8:
	- 1x1: 108 KB (108 blocks)
	- 0.5x1: 54 KB (54 blocks)
	- 1x0.5: 48 KB (48 blocks)
- 3.3V sram512x8:
	- 1x1: 90 KB (180 blocks)
	- 0.5x1: 45 KB (90 blocks)
	- 1x0.5: 42 KB (84 blocks)
- 3.3V sram256x8:
	- 1x1: 66 KB (264 blocks)
	- 0.5x1: 33 KB (132 blocks)
	- 1x0.5: 33 KB (132 blocks)

Capacity-interpretation caveat from wafer.space density report summaries:

- Theoretical values assume ideal packing/routing.
- Real designs pay heavy overhead for taps/fillers/PDN/clock/routing.
- Run 1 practical occupancy often lands well below theoretical maxima.

## Design Help Ecosystem (wafer.space design-help)

wafer.space design-help page positions service providers as independent.

Important disclaimer on that page:

- Services are listed as not affiliated with or endorsed by wafer.space.

Listed provider categories include:

- Physical verification support (LVS/ERC focus)
- End-to-end ASIC platform/services
- AI + chip design consultancy
- OpenROAD commercial support/customization
- Mixed-signal IC design consulting

Named providers shown on design-help page:

- D. Mitch Bailey:
	- physical verification support for GF180 projects,
	- focused on LVS/ERC diagnosis and reliability/ERC flow debugging,
	- developer of CVC-RV,
	- engagement model includes initial exploration + hourly ongoing support.
- ChipFlow:
	- ASIC platform/services with self-service and collaborative/full-service modes,
	- positions pre-verified references and broader design acceleration support.
- Mabrains:
	- AI + chip design consultancy,
	- broad semiconductor services with open-source ecosystem experience,
	- explicit references to GF180 engagement capability.
- Precision Innovations:
	- OpenROAD development company,
	- offers commercial support, custom tooling, and training,
	- positioned for bug-fix and flow-feature support on production schedules.
- Slice Semiconductor:
	- mixed-signal IC consulting/services,
	- offers full ASIC development and consulting lanes across applications.

Community links shown in wafer.space pages:

- Discord support path for peer/community assistance
- Project template and GF180 docs links

Design-help page maturity note:

- Community forum/documentation/tools blocks are presented with "coming soon"
	wording on that page, so prefer Discord/GitHub/template docs for immediate
	practical support.

## Submission and Sign-off Contract for ColdFoot RevA

Based on wafer.space flow descriptions and template docs, treat these as
mandatory gates before submission:

1. Final top-level GDSII generated from selected slot configuration.
2. Clean or reviewed DRC/LVS/ERC status with documented exceptions.
3. Pad-ring decision frozen:
	 - default pad ring if using COB add-on,
	 - custom/no pad ring only when package/bring-up plan supports it.
4. Power/ground pad intent preserved and reviewed against package assumptions.
5. Slot dimensions and IO mapping frozen to purchased slot type.
6. Precheck run completed (template references gf180mcu-precheck).
7. Archive submission bundle with reproducibility metadata:
	 run configs, versions, reports, and final views.

## OCD SRAM DRC waiver policy (ColdFoot RevA)

The `gf180mcu_ocd_ip_sram__*` macros (open-source community rebuild of the
GF180MCU OCD single-port SRAM family) ship with a known set of vendor-internal
DRC markers that originate inside the macro footprint and cannot be repaired
from user RTL or PnR. They are accepted under item 2 above as
"documented exceptions" subject to the following policy:

1. **Scope.** Only DRC errors whose Magic `cellname` or KLayout `<cell>`
	 reference falls strictly inside an instance of
	 `gf180mcu_ocd_ip_sram__sram256x8m8wm1`,
	 `gf180mcu_ocd_ip_sram__sram512x8m8wm1`, or
	 `gf180mcu_ocd_ip_sram__sram1024x8m8wm1` are waived. User-routed metal
	 (top-level signal nets, PDN above the macro, pad-ring routing) must
	 remain clean.
2. **Tool configuration.** Set `MAGIC_DRC_USE_GDS: False` in
	 `asic/librelane/config.yaml` so Magic DRC consumes the LEF abstract
	 view of each macro rather than the macro GDS. Keep `MAGIC_GDS_FLATGLOB`
	 entries for I/O contact cells only; do not flatten OCD SRAM cells.
	 For KLayout DRC there is no native per-cell skip — filter the
	 generated `drc.klayout.lyrdb` post-flow by cell hierarchy path before
	 building the submission summary.
3. **Submission archive must include**
	 - vendor IP version (`gf180mcu_ocd_ip_sram` repo SHA or release tag),
	 - the unfiltered `*.drc.rpt` (Magic) and `drc.klayout.lyrdb` (KLayout)
		 from the final flow run,
	 - the filtered "user-side" headline counts derived as above,
	 - a per-rule waiver list grouped by Magic rule code and KLayout
		 marker layer,
	 - confirmation that the macro GDS was used unmodified (e.g. SHA of
		 the consumed `.gds` files in `gf180mcu_ocd_ip_sram/gds/`).
4. **Cross-references.**
	 - Closure status and exact residual counts:
		 `docs/signoff_report_optionB.md`.
	 - Most recent tile synthesis report applying this policy:
		 `docs/tile_synth_report_15_43_06_run10.md` (§9).
	 - LibreLane config knob: `asic/librelane/config.yaml`
		 (`MAGIC_DRC_USE_GDS` block).

## Local GF180 project template summary (full scan)

Wafer.Space project template root scanned: `asic/`

### Top-level template structure and intent

- `README.md`: usage runbook for PDK clone, LibreLane run, simulation, slots.
- `Makefile`: primary command interface.
- `flake.nix` and `shell.nix`: reproducible Nix development shell.
- `src/`: chip top/core RTL and slot macro defines.
- `librelane/`: flow config, SDC, PDN Tcl, slot YAML files.
- `cocotb/`: simulation testbench runner and default test.
- `scripts/`: helper scripts (`padring.py`, `lay2img.py`).
- `ip/`: local logo/ID macro views and generation helper.
- `.github/`: CI flow and custom actions for Nix setup/build.

### Makefile command model

Template `Makefile` supports:

- `make clone-pdk`:
	- clones wafer-space GF180 fork at `PDK_TAG` (default 1.8.0)
- `make librelane`:
	- runs full implementation with selected slot YAML and config
- `make librelane-nodrc`, `make librelane-klayoutdrc`,
	`make librelane-magicdrc`:
	- development shortcuts with selective check skipping
- `make librelane-openroad`, `make librelane-klayout`:
	- open last run in GUI
- `make librelane-padring`:
	- runs a padring-only custom flow via Python
- `make sim` and `make sim-gl`:
	- cocotb RTL and gate-level sims
- `make sim-view`:
	- waveform view in GTKWave
- `make render-image`:
	- renders layout preview PNGs via KLayout Python API

Slot selection in Makefile:

- `AVAILABLE_SLOTS = 1x1 0p5x1 1x0p5 0p5x0p5`
- default slot is `1x1`
- override by `SLOT=<slot>`

### LibreLane config highlights (`librelane/config.yaml`)

- Flow metadata version 3 with `Chip` flow.
- Design sources: `src/chip_top.sv`, `src/chip_core_tile.sv`.
- SDC wired to `librelane/chip_top.sdc` for PNR and signoff.
- Power nets fixed to `VDD` and `VSS`.
- Clock port/net configured as `clk_PAD` / `clk_pad/Y`, period 40ns.
- Uses `PRIMARY_GDSII_STREAMOUT_TOOL: klayout`.
- Includes macro declarations for:
	- local marker macros (`gf180mcu_ws_ip__id`, `gf180mcu_ws_ip__logo`)
	- GF180 SRAM macro (`gf180mcu_fd_ip_sram__sram512x8m8wm1`)
- Places two SRAM instances with explicit locations/orientations.
- Defines explicit `PDN_MACRO_CONNECTIONS` for SRAMs.
- Includes DRC/flattening caveats and workaround settings.
- `ERROR_ON_MAGIC_DRC: False` in template defaults (note in risk section below).

### Slot YAMLs (`librelane/slots/*.yaml`)

Slot files define:

- absolute `DIE_AREA` and `CORE_AREA`,
- `VERILOG_DEFINES` selecting slot family macro,
- fixed pad-instance placement lists per edge.

Provided slot files:

- `slot_1x1.yaml`
- `slot_0p5x1.yaml`
- `slot_1x0p5.yaml`
- `slot_0p5x0p5.yaml`

### Slot-to-RTL pad count coupling

`src/slot_defines.svh` maps each slot macro to pad count constants:

- `SLOT_1X1`: 8 DVDD, 10 DVSS, 12 input, 40 bidir, 2 analog
- `SLOT_0P5X1`: 8 DVDD, 8 DVSS, 4 input, 44 bidir, 6 analog
- `SLOT_1X0P5`: 8 DVDD, 8 DVSS, 4 input, 46 bidir, 4 analog
- `SLOT_0P5X0P5`: 4 DVDD, 4 DVSS, 4 input, 38 bidir, 4 analog

### RTL template architecture

`src/chip_top.sv`:

- Instantiates GF180 IO pads:
	- `gf180mcu_fd_io__in_s` for clock
	- `gf180mcu_fd_io__in_c` for reset/input pads
	- `gf180mcu_fd_io__bi_24t` for bidir pads
	- `gf180mcu_fd_io__asig_5p0` for analog pads
- Instantiates wafer.space power pad wrappers:
	- `gf180mcu_ws_io__dvdd`, `gf180mcu_ws_io__dvss`
- Instantiates `chip_core` and marker macros:
	- `gf180mcu_ws_ip__id` (explicitly marked as required)
	- `gf180mcu_ws_ip__logo` (optional in template note)

`src/chip_core_tile.sv`:

- Example logic: ColdFoot_SoC.

### PDN and timing collateral

`librelane/pdn_cfg.tcl`:

- Custom PDN logic for std-cell grid and macro grids.
- Special macro PDN handling for two SRAM orientations.
- Extra Metal4 straps to improve macro/top-level PDN connectivity.

`librelane/chip_top.sdc`:

- Applies clock constraints, IO delays, fanout/load/uncertainty, derates.
- Handles bidir and input port timing constraints.

### Simulation harness (`cocotb/chip_top_tb.py`)

- Supports RTL and GL sim modes.
- Runner defaults:
	- simulator: icarus
	- PDK root env var
	- slot-driven compile define (`SLOT_<...>`)
- GL mode adds:
	- SCL verilog models
	- powered netlist from `final/pnl/chip_top.pnl.v`
	- `FUNCTIONAL` and `USE_POWER_PINS` defines
- Includes IO model and SRAM model from PDK paths.
- Includes local marker macro verilog stubs.
- Simple regression checks counter behavior after reset/startup.

### Utility scripts

`scripts/padring.py`:

- Defines a reduced custom LibreLane sequential flow to generate padring-only
	outputs (no full digital implementation path).

`scripts/lay2img.py`:

- Loads GDS and PDK LYP, filters layers, renders white/black PNG previews.

### Local IP marker macros

`ip/gf180mcu_ws_ip__id/*` and `ip/gf180mcu_ws_ip__logo/*` include:

- GDS, LEF, LIB, and Verilog abstraction files.
- Verilog stubs are empty module shells (layout-only identity/branding).
- LEF indicates block obstructions across Metal1..Metal5.

`ip/gf180mcu_ws_ip__logo/script/make_gds.py` and local `Makefile`:

- Convert logo image to GDS polygons with selected layers.
- Run KLayout DRC check for generated logo macro.

### CI/Nix behavior

`flake.nix`:

- pins LibreLane input (`github:librelane/librelane/3.0.0`)
- provisions dev shell tools:
	gnumake/grep/awk, iverilog, verilator, gtkwave, surfer,
	python packages including cocotb/docopt/pillow.

`.github/workflows/ci.yml`:

- matrix over slots when running in official upstream template repo,
- runs clone-pdk, sim, implementation, image render, and sim-gl,
- uploads GDS/images as artifacts.

## Local ColdFoot reality check (this workspace)

Observed in this workspace during this update:

- `.pdks/gf180mcu` directory at workspace root: 
- `asic/gf180mcu` local PDK clone: present with expected structure and content.

Implications for immediate runs:

- Do not expect local GF180 flow success until a valid `PDK_ROOT` is present.
- For `asic/` template commands, run `make clone-pdk` or point to a populated
	external PDK root.

## RevA Tape-in/Tape-out execution checklist

1. Confirm purchased slot type and pricing lane (early/standard).
2. Freeze slot geometry and pad strategy (default vs custom pad ring).
3. If COB add-on is planned, keep default pad ring compatibility.
4. Ensure local PDK assets exist and match selected PDK/SCL.
5. Run implementation and collect final views/reports.
6. Run RTL and GL sims with current final netlist.
7. Run precheck and sign-off checks (DRC/LVS/ERC).
8. Review all waived/known violations and document rationale.
9. Produce submission package with GDS and supporting evidence.
10. Submit before run deadline (AoE-based cutoff).

## Recommended command patterns

### Template-style standalone ASIC flow

```sh
cd asic
make clone-pdk
make librelane
make sim
make sim-gl
```

### Slot override example

```sh
cd asic
SLOT=0p5x1 make librelane
```

### Padring-only build (analog-oriented planning)

```sh
cd asic
make librelane-padring
```

### Layout preview generation

```sh
cd asic
make render-image
```

## Risks and caveats to track explicitly

- Template defaults may suppress hard-fail on Magic DRC
	(`ERROR_ON_MAGIC_DRC: False`); do not confuse that with sign-off closure.
- Slot docs include quarter-slot technical definitions even if not always listed
	in active purchase options.
- CNX article values and campaign dates are useful context, but always use
	current wafer.space pages for live commitments.
- Capacity planning must account for real congestion/PDN/clock overhead,
	not only theoretical per-mm2 cell limits.

## Source anchors

Local files:

- `.github/instructions/gf180mcu.instuctions.md`
- `asic/README.md`
- `asic/Makefile`
- `asic/librelane/config.yaml`
- `asic/librelane/chip_top.sdc`
- `asic/librelane/pdn_cfg.tcl`
- `asic/librelane/slots/slot_1x1.yaml`
- `asic/librelane/slots/slot_0p5x1.yaml`
- `asic/librelane/slots/slot_1x0p5.yaml`
- `asic/librelane/slots/slot_0p5x0p5.yaml`
- `asic/src/chip_top.sv`
- `asic/src/chip_core_tile.sv`
- `asic/src/slot_defines.svh`
- `asic/cocotb/chip_top_tb.py`
- `asic/scripts/padring.py`
- `asic/scripts/lay2img.py`

## Update policy

When updating this file:

1. Re-check slot docs and `slots.json` values for geometry/pad changes.
2. Re-check active run deadlines and add-on pricing.
3. Re-check local template structure and flow defaults if template revisions land.
4. Keep GF180 technology details delegated to
	 `.github/instructions/gf180mcu.instuctions.md`.
