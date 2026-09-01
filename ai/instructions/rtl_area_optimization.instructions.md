---
description: "Use when optimizing ColdFoot SNN tile RTL (chip_core_tile.sv and all sub-modules) for minimum digital cell/FF/area and maximum effective SRAM utilization on a wafer.space GF180 1x1 die slot. Required reading whenever editing RTL under hw/ip/tile/, hw/ip/logical_neuron/, hw/ip/neuron_compute/, hw/common/packages/, hw/common/mem/, or asic/src/chip_core_tile.sv."
applyTo: "asic/src/chip_core_tile.sv,hw/ip/tile/src/**,hw/ip/logical_neuron/src/**,hw/ip/neuron_compute/src/**,hw/common/packages/**,hw/common/mem/**,hw/common/struct/**,hw/common/rv_lib/**,hw/common/arb/**,hw/common/interfaces/**"
---

# ColdFoot Tile — RTL Area Optimization Instructions

## 0. Scope and Mission

You are optimizing the RTL hierarchy rooted at
[asic/src/chip_core_tile.sv](asic/src/chip_core_tile.sv) — i.e. `tile_top`
plus every sub-module it instantiates (ingress, host_io, event/egress queue
banks, dispatch scheduler, fanout executor, synapse bank, NoC egress,
`logical_neuron_*` banks, `neuron_compute_core`, shared `mem_*` wrappers,
RV interfaces, and packages).

The goal is a **CMOS ASIC implementation of a Spiking Neural Network** that
maximizes the number of programmable neurons and synapses per
**wafer.space 1x1 die slot** (12.92 mm² core), under the following floorplan
contract:

| Lane            | Budget (of 12.92 mm² core) | Owner                              |
| --------------- | -------------------------- | ---------------------------------- |
| SRAM macros     | 30 % (~3.876 mm²)          | This RTL (neurons + synapses)      |
| Digital logic   | 20 % (~2.584 mm²)          | This RTL (control + datapath FFs)  |
| Reserved        | 50 %                       | Other IPs / PDN / margin           |

Every optimization MUST be justified against one of:
**(a)** fewer standard cells, **(b)** fewer flip-flops, **(c)** less SRAM
macro area, or **(d)** more neurons/synapses per byte of SRAM. Anything that
does not move one of those four needles is out of scope for this workflow.

## 1. Invariants (Do Not Break)

These are non-negotiable. Any patch that violates one of them is rejected.

1. **Spike envelope is frozen.** The packed struct
   [`tile_in_spike_t`](hw/common/packages/tile_flit_types.vh#L46) — together
   with its aliases `tile_spike_t`, `tile_out_spike_t`, and the underlying
   `tile_queue_event_t` — stays at exactly **24 bits / 3 bytes**.
   - Sub-field widths (`WEIGHT_W`, `EVENT_TIME_W`, `NEURON_LOCAL_W`) are
     allowed to flex within that 24-bit envelope, gated by
     `TILE_QUEUE_EVENT_LAYOUT_VALID` and `WEIGHT_PROFILE_VALID` in
     [hw/common/packages/tile_pkg.sv](hw/common/packages/tile_pkg.sv).
   - Source-tile identity must NOT be added inside the 24 b body.
2. **Neuron ISA feature set is preserved.** Every `OP_*` opcode declared in
   `tile_pkg` (LDI, RECV, ACCUM_W, LEAK, INTEG, SPIKE_IF_GE, RESET, REFRACT,
   EMIT, TDEC, TINC, STDP_LITE) must remain functionally executable end-to-end
   through `neuron_compute_core`. You may shrink opcode width, repack the
   encoding, or fold mutually-exclusive ops into a denser table, but you may
   not delete a feature.
3. **End-to-end behavior is preserved.** Programming channels
   (`prog_state_if`, `prog_ctx_if`, `prog_ucode_if`, `prog_syn_if`), runtime
   spike path (ingress → event queue → dispatch → worker → fanout →
   egress queue → NoC egress), and the host response path
   (ingress → host_io → host) must each remain functional.
4. **Ready/valid contract.** All module boundaries continue to use the
   `rv_if` interface with grouped packed-struct payloads. No ad-hoc scalar
   bundles at boundaries.
5. **No silent feature loss.** If you remove a packet kind, CSR, or
   class-id, you must prove (by grep) it has zero readers in the maintained
   RTL and runtime/sw paths under `sw/runtime/`.

## 2. Mandatory Workflow

### 2.1 Plan first

Before touching RTL, **read or update** the live TODO at
[docs/rtl_area_optimization_todo.md](docs/rtl_area_optimization_todo.md).
Use `manage_todo_list` to mirror the file's open items into the session
tracker. Mark exactly one item `in-progress`; complete it; then move on.

### 2.2 Source of truth order

1. RTL in `hw/` and `asic/src/` (authoritative).
2. `tile_pkg.sv` parameters and `tile_flit_types.vh` typedefs.
3. `docs/optimization_rtl_report_tile.md` and
   `hw/ip/tile/docs/tile_architecture_rtl.md` are **legacy reference only** —
   read for ideas, never cite as ground truth.
4. Git history only on explicit user request.

### 2.3 Per-module gate

For every module you touch, all of these must pass before you mark the TODO
item complete:

- The module compiles via the wafer.space lane (use the `ws_flow_dbg` agent
  or `tools/flows/tool_flow.py compile --project tile`).
- Yosys synth area (cells + sequential bits) for that module is **≤** the
  pre-edit baseline. Capture the delta in the TODO file.
- Any width or struct change has a corresponding `tile_pkg` localparam and a
  compile-time `localparam bit *_LAYOUT_VALID` guard, mirroring the existing
  `TILE_QUEUE_EVENT_LAYOUT_VALID` / `FANOUT_ROUTE_LAYOUT_VALID` style.
- Inline SVA under `` `ifndef SYNTHESIS `` is preserved or extended; never
  deleted to silence a warning.

## 3. Optimization Patterns (Apply in This Order)

The order is chosen so each pass reduces what the next pass has to touch.

### Pass A — Parameterize and rename to package-derived widths

- Every literal width in a sub-module signal/port (`logic [7:0]`,
  `logic [3:0]`, `[15:0]`, etc.) must become a `tile_pkg` parameter or a
  derivation thereof. Add the parameter to `tile_pkg.sv` if it does not yet
  exist; never invent a new magic constant inside a leaf module.
- Rename signals and ports to reflect the package term that drives them, e.g.
  `wire [7:0] route_byte` → `logic [FANOUT_ROUTE_W-1:0] route_w`. The aim is
  that *reading the leaf module tells you which packet field a wire belongs
  to*.
- Free to rename structs **except** the spike family from §1.1. When you
  rename, update every consumer in one commit and re-run elaboration.

### Pass B — Shrink and merge non-spike packets

- Audit every `typedef struct packed` in
  [hw/common/packages/tile_flit_types.vh](hw/common/packages/tile_flit_types.vh).
  For each field, ask: *is this width consumed anywhere?* If not, drop it
  and update writers/readers.
- Merge `neuron_state_rd_rsp_t` fields that are co-emitted by the loader
  (`ucode_ptr`, `ucode_len`, `fanout_ptr`, `fanout_len`) into the smallest
  legal widths derived from `NEURONS_PER_TILE`, `SYNAPSE_SRAM_DEPTH`, and
  `PROG_IDX_W`. Do not pad to byte boundaries until the SRAM-mapping pass
  (Pass D) needs it.
- `message_packet_t` (the full host/mesh frame) is much wider than
  `message_packet_min_t`. Inside the tile, only the `min` form is allowed
  past `tile_ingress`. Confirm the full form never crosses a tile-internal
  boundary; if it does, narrow it at the boundary.
- Collapse `union packed { spike; cfg; }`-style payloads only when both
  arms fit in the same envelope; otherwise keep the discriminator
  (`payload_is_spike`) and shrink the wider arm first.

### Pass C — Trim ISA encoding and worker datapath

- `NEURON_OP_W = 6` (64 opcodes) supports only 12 real ops today. Reduce to
  the smallest power-of-two that fits the live opcode set plus a documented
  reserve (target `NEURON_OP_W = 4`). Touch all four sites: package, ucode
  bank row width, worker decode, programming path.
- Fold `RF_COUNT` and `RF_REG_W` into one `RF_FLAT_W` actually consumed by
  the ALU; per-register storage that is never selected individually should
  collapse to a single accumulator register.
- Replace any divide/multiply by a constant in `neuron_compute_core` with
  shift+add. Replace any `case` of mutually-exclusive comparisons with a
  priority encoder when it shortens the cone of logic.
- Gate worker FF clocks (`ena &&` guard on every `always_ff`) so quiet
  neurons don't toggle.

### Pass D — SRAM macro mapping for the 30 % lane

The SRAM area budget is `WS_1X1_SRAM_BUDGET_UM2 = 3 876 000 µm²`. Macros
available:

| Library | Macro          | µm² each | bits each | bits / mm² |
| ------- | -------------- | -------: | --------: | ---------: |
| FD      | `sram128x8`    | 116 100  |     1 024 |      8 820 |
| FD      | `sram512x8`    | 209 400  |     4 096 |     19 564 |
| OCD     | `sram512x8`    |  97 000  |     4 096 |     42 226 |
| OCD     | `sram1024x8`   | 155 400  |     8 192 |     52 717 |

Rules:

1. Default banking choice is **OCD `sram512x8`** (highest bits/mm²) unless
   the bank's depth × width naturally fits an OCD `sram1024x8` better.
2. A bank's *row width* MUST be a multiple of 8 b. Round up by repacking
   the smallest field — never by adding a reserved field that no consumer
   reads.
3. A bank's *depth* SHOULD be a multiple of 512 (or 1024 for OCD-1k) so no
   macro is left half-empty. If the natural depth is < 512, prefer FF
   storage and exclude the bank from the SRAM budget.
4. **Synapse bank** is the dominant cost. Target row =
   `FANOUT_ROUTE_W + NEURON_IDX_W + WEIGHT_W` rounded up to 16 b (two byte
   lanes). With `WEIGHT_W = 4` the row fits one byte lane → halves
   `sram512x8` count vs `WEIGHT_W = 8`. Expose this trade-off via a
   `tile_pkg` parameter and document the chosen profile in the TODO.
5. **State + context banks** are per-neuron. Merge them into a single
   `logical_neuron_state_context_bank` if their write ports can share a
   port without arbitration penalty; otherwise keep them split and pack
   each row into one byte lane.
6. **Ucode bank** width is fixed at 16 b. Pack 4-row strides per
   `sram512x8` so `PROG_IDX_W = 5` (32 words/neuron) maps to a clean
   `NEURONS_PER_TILE × 32 / 256` macro count.
7. Compile-time assert SRAM macro count × per-macro area ≤
   `WS_1X1_SRAM_BUDGET_UM2`. Failure must be a `$fatal` at elaboration, not
   a synth-time surprise.

### Pass E — Logic budget (20 % lane)

- After Pass A–D, run synth and capture standard-cell area per module.
- For any module above its share of 2.584 mm² (proportional to its
  per-event activity), apply in order: shared decoders, one-hot → binary
  encoding, retiming, FSM state minimization, redundant register removal.
- Replace any `for` loop over `NEURONS_PER_TILE` that becomes muxes wider
  than 8 inputs with a sequential walker (one neuron per cycle). The
  scheduler already throttles event rate, so a 1-cycle-per-neuron walker is
  free on throughput.

### Pass F — Verification gate

- Elaborate the wafer.space ASIC target: `chip_core_tile.sv` + tile RTL.
- Run any cocotb tests that exist under `hw/ip/tile/test/`. Tests must pass
  with the same seed before and after each pass.
- Synthesize and capture: total stdcell area, total FF count, total macro
  count and area, per-module top-10 by area. Write the delta to the TODO
  file under the entry for that pass.

## 4. Renaming Conventions

When renaming signals/structs/ports during Pass A:

- Suffix `_w` for widths, `_n` for active-low, `_q` for registered, `_d` for
  next-state combinational, `_if` for `rv_if` instances.
- Prefix by domain: `syn_*` (synapse bank), `nrn_*` (neuron state/context),
  `evq_*` (event queue), `egq_*` (egress queue), `ucd_*` (ucode), `rt_*`
  (route fields), `pkt_*` (packet/message), `wkr_*` (worker).
- Struct types end in `_t`. Enum types end in `_e`. Parameter names are
  `UPPER_SNAKE`. Localparams visible only inside a module may be
  `UpperCamel` to distinguish them from package-exported parameters.
- Never name a wire after its bit width.

## 5. Anti-Patterns (Reject on Sight)

- Hardcoded `8`, `16`, `24`, `32` in any leaf module's port or signal
  declaration. Always go through `tile_pkg`.
- New ad-hoc concatenations across module boundaries (`{a, b, c}` carrying
  semantics that should live in a packed struct).
- Comments that justify keeping width X "for future use" without a
  matching consumer in this branch.
- Adding a reserved field inside the 24 b spike envelope.
- `generate` blocks that instantiate per-neuron logic instead of using a
  single shared datapath plus the SRAM-backed state.
- Deleting SVA to make a tool quiet.
- Padding SRAM banks to a power of two when nothing reads the extra rows.

## 6. Deliverables Per Session

1. Updated RTL with all changes contained to the files in `applyTo`.
2. Updated `tile_pkg.sv` parameters (with `_LAYOUT_VALID` guards) and
   `tile_flit_types.vh` typedefs.
3. Updated [docs/rtl_area_optimization_todo.md](docs/rtl_area_optimization_todo.md)
   with checked items, before/after numbers, and any new items discovered.
4. A short note in the session memory (`/memories/session/`) summarizing
   which pass(es) were executed and what blocked the next one.

No new markdown documentation files are produced unless the user explicitly
asks. The TODO file is the single living artifact for this workflow.
