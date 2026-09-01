# Formal Verification — Coverage Map and Audit Findings

## Scope

What's already covered by SymbiYosys / Yosys formal flows, what's *not*,
and what got added in the Pass-W refactor (multi-worker tiles + pipelined
decay).  Read alongside `ai/instructions/snn_asic.instructions.md` and the
per-IP arch docs under `hw/ip/*/docs/`.

## Where formal lives

| Per-IP directory                                  | Targets                                                                 |
|---------------------------------------------------|-------------------------------------------------------------------------|
| `hw/ip/logical_neuron/formal/`                    | `logical_neuron_ucode_bank_formal`, `logical_neuron_state_ctx_bank_formal` |
| `hw/ip/neuron_compute/formal/`                    | `neuron_compute_core_formal`, `neuron_exec_formal_*` (7 per-property task suites) |
| `hw/ip/tile/formal/`                              | `tile_dispatch_scheduler_formal` (10 task suites), `tile_event_queue_bank_formal`, `tile_fanout_executor_formal`, `tile_ingress_formal`, `tile_ping_responder_formal`, `tile_noc_egress_formal`, `tile_host_io_formal`, `tile_top_formal`, `tile_top_packed_formal` |
| `hw/ip/neural_mesh/formal/`                       | `mesh_router_*`, `mesh_bundle_loader_*`, `flit_framing_formal_*`, `packet_flit_roundtrip_*` |
| `hw/ip/host_gateway/formal/`                      | `io_egress_*`, `io_response_*`, `io_graph_clear_*`, `host_shim_*` |
| `hw/common/formal/`                               | `rv_lib_*`, `mem_*`, `arb_*` |

Run via `tools/dev/flow formal <ip>` or directly with `sby -f <target>.sby`.
Top-level convention: `<dir>/<target>_formal.sby` is the maintained config,
`<dir>/<target>_formal.sv` is the harness, and `<dir>/<target>_formal_<task>/`
is the **sby workdir** (auto-generated; not source).

## What bugs the existing formal would *not* have caught

Bugs landed during Pass-W bring-up (May 18 2026), classified by formal
catchability:

| Bug                                                                                       | Class                | Formal-catchable? |
|-------------------------------------------------------------------------------------------|----------------------|-------------------|
| `tools/flows/fpga_program.py` hard-coded `-cs_url TCP:localhost:3042` failing             | Flow / TCL           | No                |
| `tile_top.WORKER_CORES_CFG = 1` hard-coded (FPGA wanted 2)                                | Parameter plumbing   | Indirectly — `cover` that `WORKER_CORES > 1` is reachable would have flagged |
| `tile_dispatch_scheduler.worker_rr_r` stubbed to always-0                                 | Stub-as-scaffolding  | **Yes** — `cov_worker_rr_advances` proves all workers eventually selected |
| Single-worker `event_r → rf_r` 38-LUT-level path inside `neuron_exec`                     | Timing closure       | No (PnR-time)     |
| `decay_with_dt` 8-iter cascade exceeding the 25 ns budget at multi-worker placement      | Timing closure       | No                |
| FPGA `nexys_video_base.xdc` UART pin direction confusion                                  | Board-level XDC      | No                |
| `btnc` floating high → `!btnc` → reset held forever                                       | Board-level / XDC    | No                |
| FT2232 channel B `LoadVCP=1` not enumerating FTDIPORT child                               | Windows driver       | No                |
| MMCM not locking / `sys_clk` not toggling (still being diagnosed)                         | Mixed-signal         | No                |

**Takeaway:** formal is silent on board-level / I/O / mixed-signal /
flow-tooling failures.  It's strong on per-IP RTL invariants and stub
detection (parameters / flags wired but unused).

## What got broken by the Pass-W refactor

The multi-worker + pipelined-decay refactor touched four maintained source
files and broke the existing formal harness elaboration in three places.

| Harness                                                              | Why it broke                                                                                       | Fix |
|----------------------------------------------------------------------|----------------------------------------------------------------------------------------------------|-----|
| `tile/formal/tile_dispatch_scheduler_formal.sv`                      | DUT ports `worker_start_if` / `worker_result_if` now arrays of `WORKER_CORES_P`                    | Wire size-1 array; add a `WORKER_CORES_P=2` task variant |
| `logical_neuron/formal/logical_neuron_ucode_bank_formal.sv`          | DUT ports `req_if` / `rsp_if` now arrays of `READ_PORTS`                                           | Wire size-1 array; add a `READ_PORTS=2` task variant |
| `neuron_compute/formal/neuron_compute_core_formal.sv`                | DUT FSM split `S_EXEC` → `S_EXEC1`/`S_EXEC2`; some inline assertions still reference old `S_EXEC`  | Update internal refs; verify pipeline correctness |
| `neuron_compute/formal/neuron_exec_formal*.sv` (7 task suites)       | DUT gained input `decay_result_in` (must be driven by harness)                                     | Drive `decay_result_in` symbolically; the per-task properties around LEAK/TDEC need the new wiring |

## New properties added in this pass

### Scheduler — multi-worker invariants (per-worker RR + result mux)

In `hw/ip/tile/src/tile_dispatch_scheduler.sv` (`\`ifdef FORMAL` block):

- **`ARC-DS-07 worker_rr_pointer_walks`** — Cover that `worker_rr_r` reaches
  every value in `[0, WORKER_CORES_P)` over a bounded trace.  Catches the
  "stuck-at-0" stub that originally prevented multi-worker dispatch.

- **`ARC-DS-08 dispatch_one_lane`** — At most one `worker_start_if[k].valid`
  is high in any cycle (the RR selector is mutually exclusive).  Guards
  against accidentally double-dispatching the same event.

- **`SNN-DS-04 result_payload_matches_selected_lane`** — When
  `commit_out_if.valid` is high, `commit_out_if.rv_payload` equals
  `worker_result_if[sel_result_idx_c].rv_payload`.  Guards the priority
  encoder result mux against MUX-select faults.

- **`SNN-DS-05 per_neuron_serialization_holds_with_multi_worker`** — Two
  events for the same `neuron_idx` are never in flight simultaneously,
  even across worker lanes.  The `inflight_r` bitmap is the gate; this
  proves it survives the RR split.

### Ucode bank — multi-port invariants (lockstep writes, independent reads)

In `hw/ip/logical_neuron/src/logical_neuron_ucode_bank.sv`:

- **`ARC-UC-13 port_independence`** — Read activity on port `i` does not
  affect `rsp_pending_r` / `rsp_have_capture_r` of port `j ≠ i`.  Guards
  against accidentally sharing per-port staging state.

- **`ARC-UC-14 write_fans_out_lockstep`** — When `prog_wr_if` fires, every
  per-port memory copy receives an identical write that cycle (same
  address, same data, same byte-enable).  Guards against the per-port
  generate loop accidentally gating writes per-port.

### neuron_compute_core — pipelined decay invariants

In `hw/ip/neuron_compute/src/neuron_compute_core.sv` (S_EXEC1 → S_EXEC2):

- **`ARC-NC-12 decay_pipeline_no_leak`** — In `S_EXEC2`, the registered
  pipeline values `decay_mid_r` / `decay_shift_pipe_r` / `decay_dt_pipe_r`
  equal whatever `decay_mid_c` / `decay_shift_c` / `decay_dt_c` were in
  the preceding `S_EXEC1` cycle.  Guards against pipeline staleness.

- **`ARC-NC-13 exec2_implies_exec1_predecessor`** — `state_r == S_EXEC2`
  is only ever reached from `state_r == S_EXEC1` (FSM enforces the
  pipeline depth).

- **`SNN-NC-04 commit_uses_pipelined_decay`** — When an OP_LEAK or OP_TDEC
  ucode word is the current `instr_r`, the value written into `rf_r` at
  the `S_EXEC2 → next` transition equals `decay_with_dt_half_b(decay_mid_r,
  decay_shift_pipe_r, decay_dt_pipe_r)` for the lane the opcode targets.
  Closes the loop on "the pipelined decay reaches the FF".

## How to run the new properties

```sh
# From within the wafer.space container (sby + yosys on PATH):
tools/dev/flow formal tile           # all tile_dispatch_scheduler tasks
tools/dev/flow formal neuron_compute # all neuron_compute_core + neuron_exec tasks
tools/dev/flow formal logical_neuron # ucode bank + state_ctx bank tasks

# Or directly:
cd hw/ip/tile/formal && sby -f tile_dispatch_scheduler_formal.sby bitmap_invariants
```

The default harness instantiations use `WORKER_CORES_P=1` / `READ_PORTS=1`
so existing properties continue to prove cleanly.  The new
`*_multi_worker` task variants exercise `WORKER_CORES_P=2` / `READ_PORTS=2`
to cover the new arrayed semantics.

## Update policy

When a new architectural mechanism lands:

1. Add inline `\`ifdef FORMAL` assertions to the maintained source.
2. Update the per-IP `<target>_formal.sv` harness if the DUT port list
   changed.
3. Add a `cover` witness for the new invariant.
4. List the property here (`What got broken`, `New properties added`).
5. Run the target locally and confirm BMC/prove tasks close at the chosen
   depth.

Formal is **not** a substitute for cocotb on path/throughput properties,
nor for FPGA bring-up on board-level signals.  Its job is per-IP
correctness at the RTL boundary — and to keep "stub-as-scaffolding"
parameters from silently shipping.
