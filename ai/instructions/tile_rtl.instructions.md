---
description: "Use when implementing, reviewing, or optimizing ColdFoot SNN tile RTL, tile_top, logical-neuron SRAM banks, synapse banks, fanout/event/egress queues, configurable neuron/synapse capacity, spike/flit packet limits, or wafer.space GF180 area-focused tile design."
---

# ColdFoot SNN Tile RTL Instructions

## Purpose

Use this file when creating or revising the ColdFoot Spiking Neural Network
tile RTL. The target is a configurable CMOS digital tile for the wafer.space
GF180MCU `1x1` slot, with storage dominated by SRAM macros and control/compute
implemented as optimized digital logic.

The tile must be programmable for different SNN models. Do not hardcode one
network shape when the same SRAM allocation and packet contract can support a
family of shapes.

## Mandatory Hard-Gate Workflow (Required)

This workflow is mandatory for tile RTL sessions. Do not skip gates.

### Gate 0: Plan Before Edit

- Create and maintain a TODO list before touching RTL.
- Create a session checklist from `.tmp/rtl_session_checklist_template.md`.
- List all target modules and required source docs before coding.

### Gate 1: Source Discipline

- Use local workspace RTL and local docs as implementation source of truth.
- Do not source implementation from git history unless the user explicitly
  asks for historical comparison or migration.
- If requirements are ambiguous, stop and resolve ambiguity before code edits.

### Gate 2: Per-Module Implementation Gates

For each touched module, all items below must pass before marking it complete:

- Ready/valid gate: boundary interfaces use ready/valid channels with grouped
  packetized payload structs (no ad hoc scalar bundles at module boundaries).
- SRAM gate: scalable storage remains SRAM-backed; do not replace memory arrays
  with FF-heavy storage except small control/state that is clearly justified.
- SNN correctness gate: neuron-centric firing and source-owned fanout model are
  preserved.
- Area gate: wafer.space `1x1` constraints are respected, including area-related
  static/elaboration checks and updated SRAM budget accounting when relevant.

### Gate 3: Closure And Reporting

- Update checklist status after each module change.
- Run diagnostics/verification for changed scope and capture pass/fail/blockers.
- Do not claim completion while any hard-gate item remains open.

## Source Anchors

Before implementing or reviewing tile RTL, read the useful set of
these sources:

- `docs/architecture.md`: maintained ASIC, mesh, packet, and tile ownership.
- `docs/architecdocs/architectural-flags.md`: maintained ASIC, mesh, packet, and tile ownership.
- `asic/docs/multitile_1x1024_snn_asic_architecture.md`: wafer.space-class
  tile reference architecture and sizing model.
- `docs/optimization_rtl_report_tile.md`: measured area/SRAM optimization
  notes and known area-reduction opportunities.
- `hw/ip/tile/docs/tile_architecture_rtl.md`: tile hierarchy and live module
  contracts when present.
- `hw/ip/logical_neuron/docs/logical_neuron_architecture_rtl.md`: state,
  context, and ucode bank split.
- `hw/ip/neuron_compute/docs/neuron_compute_architecture_rtl.md`: worker datapath and microcode execution model.
- `hw/common/packages/tile_pkg.sv`: canonical protocol, spike, neuron, time,
  weight, and flit width constants.
- `hw/common/packages/tile_flit_types.vh`: packet/flit/tile event payload
  structs and helper functions.
- `.github/instructions/wafer.space.instructions.md` and
  `.github/instructions/gf180mcu.instuctions.md`: load only when physical
  GF180 area, SRAM macro, LibreLane, or MPW constraints affect the task.

Treat RTL and `.core` files as source of truth. Treat older docs as design
history when they conflict with maintained RTL or manifests.

## SNN Correctness Contract

- Preserve neuron-centric firing: a logical neuron integrates input, updates
  state, checks threshold, and emits a binary spike only if its state crosses
  threshold.
- Synapses and fanout entries apply weights and routing only. They must not
  decide whether a spike propagates.
- Use source-owned sparse fanout: each source neuron stores a pointer and
  length into a shared outbound synapse bank. Do not add destination-side source
  lookup tables for the normal inference path.
- Keep dynamic neuron state, static neuron config, fanout connectivity, and
  microcode storage separate.
- Model temporal behavior explicitly with event time, delta time, decay,
  refractory state, or an equivalent documented mechanism. Do not collapse the
  tile into stateless ANN-style activation logic.
- Outputs are spike events or protocol responses. Continuous values may exist
  as internal state, not as SNN output activations.

## Area And Technology Rules

- Implement the tile as digital CMOS logic plus SRAM macros. Do not introduce
  analog neuron behavior or mixed-signal assumptions into the tile RTL.
- Prefer SRAM-backed storage for neuron state, context, ucode, fanout tables,
  event queues, loader/config storage, and any scalable per-neuron or
  per-synapse data.
- Use flip-flops only for control FSMs, ready/valid staging, small FIFOs,
  caches, counters, and timing isolation where SRAM would be unreasonable.
- Use shared memories and time-multiplexed read/write phases before replicating
  banks. Extra cycles are acceptable when they reduce GF180 area and preserve
  protocol back-pressure.
- Prefer `hw/common/mem/mem_*` wrappers and existing bytelane memory
  patterns over ad hoc register arrays.
- Keep the SRAM allocation fixed and programmable for a build profile. For the
  wafer.space `1x1` default-pad-ring profile, calculate the SRAM planning
  budget as 30% of the default-pad-ring core area: `12.92 mm2 * 0.30 =
  3.876 mm2`. Recompute only if the slot/core-area definition or pad-ring
  strategy changes.
- Budget against realistic routed area, not theoretical cell density. Include
  SRAM macro area, standard-cell area, PDN, routing, clocking, fillers, and
  congestion margin.

## Tile Naming And Storage Ownership

- Rename `tile_fanout_pool.sv` and its module to `tile_synapse_bank`. This is
  the shared-SRAM per-synapse connection table. Each entry stores the outbound
  route and weight for one synapse. It is address-agnostic storage for the
  destination list, not per-neuron metadata.
- Rename `tile_fanout_bank.sv` and its module to `tile_egress_queue_bank`. The
  old name is misleading: this block is a per-worker, post-walk, pre-egress
  spike FIFO bank. `tile_fanout_executor` expands one source spike into N
  destination spikes, enqueues them here, and `tile_noc_egress` drains them.
- Keep per-neuron fanout ranges in `logical_neuron_state_bank` as pointer plus
  length metadata only. Do not describe or implement `logical_neuron_state_bank`
  as storing synapses.
- Use this ownership split in code, docs, tests, and runtime naming:
  `logical_neuron_state_bank` stores `fanout_ptr`/`fanout_len`,
  `tile_synapse_bank` stores route/weight synapse entries, and
  `tile_egress_queue_bank` buffers already-expanded outgoing spikes.
- When performing the rename, update module names, filenames, instances,
  `.core` files, docs, cocotb/formal collateral, runtime references, and source
  anchors in one coherent change. Leave compatibility wrappers only if needed
  for an intentional migration step, and mark them temporary.
- If `tile_egress_queue_bank` feels too transport-coupled for a future design,
  acceptable alternatives are `tile_emit_queue_bank` or
  `tile_outspike_fifo_bank`; default to `tile_egress_queue_bank` for symmetry
  with `tile_event_queue_bank` and `tile_noc_egress`.

## Configurability Rules

- Treat `1x1024` logical neurons, 8-bit weights, and 4096 fanout/synapse
  entries as a proven reference point, not as a fixed architectural limit.
- Use the SRAMs allocated for any memory components of this design. If there
  is an SRAM that is 1024 deep, but only 512 entries are used in one profile,
  the remaining 512 entries can be allocated to something else, such as more
  fanout entries or more neurons per tile. Keep the design flexible and
  efficient; do not hardcode a specific number of neurons or synapses when the
  same SRAM can be allocated differently for different profiles.
- Expose or preserve parameters for neuron count, worker count, weight width,
  fanout depth, queue depth, ucode depth, event-time width, and SRAM macro
  style where the protocol and memory budget allow it.
- Let SRAM capacity and packet/flit field limits define legal configurations.
  For example, a 4-bit weight profile may trade narrower fanout rows for more
  fanout entries if the runtime bundle format, RTL packing, and readback paths
  are updated consistently.
- Do not silently truncate configured weights, neuron IDs, fanout addresses,
  timestamps, or route fields. Add static/elaboration checks when a profile
  exceeds SRAM row width, depth, packet fields, or flit body capacity.
- Keep the current single-tile-per-die assumption local to the ASIC wrapper.
  The tile RTL should remain generalized and reusable, while treating 8
  dies/tiles as the product planning ceiling unless the product target changes.
- Don't use registers or FFs for memory, instead of use SRAMs. Don't use wide combinational logic to replace multi-cycle SRAM access.

## Tile Microarchitecture Guidance

- Keep `tile_top` as the tile boundary and compose smaller modules for ingress,
  event queues, dispatch, logical-neuron banks, worker execution, fanout walk,
  host/readback, and NoC egress.
- It is acceptable to add, remove, or reshape the current phantom RTL ports when
  needed. Preserve clear ownership boundaries and update callers, `.core` files,
  tests, runtime assumptions, and docs in the same change.
- Time-multiplex a small number of physical workers across many logical neurons.
  Do not instantiate one compute datapath per logical neuron.
- Use ready/valid interfaces from `hw/common` where practical. Avoid
  combinational valid-ready loops; insert skid or register slices when needed.
- Maintain back-pressure from NoC egress, host egress, fanout walking, queue
  capacity, and memory stalls. Dropping spikes or programming words is not an
  acceptable area optimization.
- Prefer packed byte-lane row formats for SRAM efficiency. Document row layouts
  in the RTL or companion docs when they affect runtime bundle generation.
- Keep command/debug/telemetry traffic observability on the command plane and
  spike/event traffic on the data plane as defined by the shared packet/flit
  packages.

## Packet, Flit, And Mesh Constraints

- Use `tile_pkg.sv` and `tile_flit_types.vh` for field widths and payload
  shapes. Do not invent parallel packet layouts in tile RTL.
- A spike packet must preserve the configured weight semantics end-to-end. If a
  compact field cannot carry the configured weight, use the maintained body-flit
  path or revise the protocol/runtime together.
- Respect the mesh flit width, maximum body-flit count, coordinate widths,
  virtual-channel policy, urgent bit, and packet plane bits.
- Keep local tile event payloads compact, but do not pack away information that
  the neuron model or runtime readback requires.
- For inter-die or multi-tile operation, route through the mesh packet/flit
  contract. Do not expose per-neuron wires at the pad ring.
- A spike is 3B. But the fields in there are not fixed. If the weight width is reduced, the extra bits can be used for more fanout entries or more neurons per tile, if the RTL, protocol, and runtime are updated together. If the weight width is increased, the extra bits can be taken from a body flit or dropped with a protocol/runtime update. The point is to keep the contract flexible and consistent, not to hardcode a specific field width in the RTL. Hence, these dynamic field widths should be defined as parameters or calculated from parameters, with static checks to prevent illegal configurations.
  - For example not all values are possible, weight width cannot exceed the flit body width minus the route and neuron ID fields, and the number of fanout entries cannot exceed what can be indexed by the route and neuron ID fields. And the only supported options are 4-bits and 8-bits.


## Implementation Style

- Preserve existing module names and naming style where possible:
  `tile_top`, `tile_ingress`, `tile_event_queue_bank`,
  `tile_dispatch_scheduler`, `tile_synapse_bank`, `tile_fanout_executor`,
  `tile_egress_queue_bank`, `tile_noc_egress`, and
  `logical_neuron_*_bank`. Treat `tile_fanout_pool` and `tile_fanout_bank` as
  legacy names to replace when touching the tile fanout/egress path.
- Use `default_nettype none`, synthesizable SystemVerilog, explicit widths,
  and parameter guards.
- Keep hot datapaths narrow and staged. Avoid wide one-cycle muxes across all
  neurons, all synapses, or all workers.
- Favor serialized programming/readback paths over duplicated storage when the
  path is not performance-critical.
- Add comments only for non-obvious row packing, scheduling invariants,
  protocol coupling, or GF180/wafer.space area tradeoffs.
- Advise AI to create a TODO list of a top-down RTL plan before coding, and to break the implementation into a sequence of small, reviewable tasks that preserve correctness and maintain test coverage.
  - For example, start with a simple single-worker, single-neuron, no-fanout design that can fire and be read back. Then add more workers, then add more neurons, then add fanout, then add configurability, etc., while keeping the design runnable and test-covered at each step.
  - When reviewing, check the TODO list and the current implementation against the SNN correctness contract, area and technology rules, configurability rules, microarchitecture guidance, and packet/flit/mesh constraints. Provide feedback on any violations or opportunities for improvement in these areas.
  - This approach helps manage complexity, preserve correctness, and maintain a clear design history through the implementation process.
  - Feel free to create markdown files in `.tmp/` for specific submodules or features if the guidance needs to be more detailed or if there are common pitfalls to avoid.
  - On the first pass, deffine packages, flits / structures, and update RTL files with pin-outs and parameters. On the second pass, implement the logic for a simple configuration and firing path. On the third pass, add configurability, fanout, and edge cases. On the fourth pass, optimize for area and timing while preserving correctness and test coverage.
  - On the bottom of every file, add assertions for formal testing and simulation to check for protocol compliance, legal configuration, and SNN correctness invariants. This helps catch bugs early and ensures that the design meets the specified contracts.
  - When optimizing for area, consider the trade-offs between logic complexity, SRAM usage, and protocol back-pressure. For example, adding more workers may reduce the need for complex scheduling logic but increase area. Conversely, a single worker with more complex scheduling may save area but require careful design to avoid timing issues and maintain throughput.

## Verification And Closure Expectations

- When changing tile-local scheduling, event queues, fanout, or neuron bank
  semantics, update cocotb/formal/runtime collateral with the RTL change.
- Add tests for at least: local spike enqueue, same-tile fanout, remote fanout,
  back-pressure, full/empty queue behavior, configured weight width, fanout
  depth limit, readback consistency, and illegal-profile rejection.
- Run the maintained tile checks first, then broader SoC checks when packet,
  loader, runtime, or mesh-visible behavior changes.
- For area-sensitive profiles, report SRAM macro count/area separately from
  standard-cell area and state the wafer.space `1x1` budget assumption used.
