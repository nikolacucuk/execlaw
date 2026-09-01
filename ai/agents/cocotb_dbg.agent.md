---
name: cocotb_dbg
argument-hint: "cocotb test suite compile, run, waveform-assisted RTL debug, and iterative fix for any testbench in this repo"
description: "Automated cocotb debug agent: compiles and elaborates the testbench, reads failing test logs, interpolates the corresponding FST waveform, cross-references architecture docs, patches RTL, reruns, and iterates until all tests pass. Produces a final fix summary and pass table."

---

# CocotbDbg Agent

## Mission
Compile the cocotb testbench for the requested DUT, run the full test suite, and autonomously iterate through a log → waveform → docs → RTL-patch → rerun loop until every test case passes.

The agent must:
- Enforce strict **compile-before-test** gating.
- Process **every failing test** individually: log analysis → waveform interpolation → doc cross-reference → smallest RTL patch.
- Never advance past a failing test without a patch attempt and a rerun.
- Count every test in the final summary; total tests reported must equal the actual test count confirmed from the cocotb summary line (`TESTS=N`).

---

## Context Intake (Required Before Running)
Resolve before issuing any command:

| Variable | Description | Default / inference |
|---|---|---|
| `TEST_DIR` | Directory containing the Makefile and test Python file | `src/tile/test` or `test/` — infer from user request |
| `TEST_MODULE` | cocotb Python module name | `test_tile_top` (tile) or `test` (TT-level) |
| `DUT_NAME` | Top-level DUT module name | `tile_top_flat` (tile) or `tb` (TT-level) |
| `SIM` | Simulator | `verilator` (default; Icarus unsupported for this design) |
| `WAVES` | Waveform capture flag | `0` for batch runs; `1` per failing test |
| `WAVE_DIR` | Directory where FST files are written | `<TEST_DIR>/waves/` |
| `DOCS_DIR` | Architecture documentation directory | `/workspaces/tt_um_neutern_0/docs/` |
| `RTL_ROOTS` | Directories containing RTL source files | `src/tile/src/`, `src/logical_neuron/src/`, `src/neuron_compute/src/`, `src/common/` |
| `CONTAINER` | Docker execution container | `iic-osic-tools_xserver` (TT lane) or `wafer.space_gf180ns` (wafer.space lane) |
| `NIX_DSHELL` | Nix devshell bin path (wafer.space lane only) | `/nix/store/m7lfmviq5inn3qcfdspc5jizrmw9j6qy-devshell-dir/bin` |

If any value is ambiguous, infer from the directory structure; ask only if inference is impossible.

---

## Phase Order (Strict Sequential Gating)

### Phase 0 — Container and Tool Preflight

Determine the execution lane from context:

**TinyTapeout lane** — container `iic-osic-tools_xserver`:
```bash
docker exec -t iic-osic-tools_xserver /bin/bash -lc \
  "cd /foss/designs/coldfoot_soc && python3 tools/dev/flow.py sim <DUT_NAME> --sim verilator"
```

**wafer.space lane** — container `wafer.space_gf180ns` with Nix PATH:
```bash
docker exec -w /workspace/<TEST_DIR_REL> wafer.space_gf180ns bash -lc \
  'DSHELL=/nix/store/m7lfmviq5inn3qcfdspc5jizrmw9j6qy-devshell-dir/bin; \
   export PATH=$DSHELL:$PATH; \
   make SIM=verilator 2>&1 | tail -60'
```

Use the wafer.space lane when the `TEST_DIR` is under `/workspace/hw/` inside the container.
The Nix `PATH` prepend is **mandatory** in the wafer.space lane — without it, `make`,
`verilator`, and `cocotb-config` are not on PATH and the build silently fails.

### Phase 1 — Compile / Elaborate
**Command:** `cd <TEST_DIR> && make SIM=verilator`

- A `make` invocation with no `TESTCASE=` runs the full suite; compilation errors appear before any test output.
- Gate: compilation must produce **zero fatal errors** before advancing to Phase 2.
- On failure: read the last 100 lines of make output, identify the first error, apply the smallest fix (typically include path, missing source, or SystemVerilog syntax), re-run Phase 1.
- Do **not** switch to Icarus for this design — it cannot parse parameterised interface ports.

### Phase 2 — Batch Run (Baseline)
**Command:** `cd <TEST_DIR> && make SIM=verilator 2>&1 | tail -5`

- Parse the cocotb summary: `TESTS=N PASS=P FAIL=F SKIP=S`.
- Record `N` as the authoritative total test count for the session.
- If `F == 0`: all tests pass → skip to Phase 5 (Final Report).
- If `F > 0`: collect failing test names from the summary table, enter Phase 3 for each.

### Phase 3 — Per-Failing-Test Debug Loop
Repeat this loop for every test that failed. Process one test at a time.

#### Step 3a — Capture the Test Log
Run the failing test in isolation with waveform capture:
```
cd <TEST_DIR> && make SIM=verilator WAVES=1 TESTCASE=<test_name> \
    WAVE_FILE=waves/<test_name>.fst SIM_BUILD=sim_build/waves/<test_name> \
    2>&1 | tail -60
```
Extract:
- **Error message** (AssertionError text, timeout text, DIDNOTCONVERGE, X-propagation, etc.)
- **Sim time** at which the failure occurred (e.g., `at 360 ns`)
- **Signal values** mentioned in the assert message
- **Log line** `WAVE_PATH=waves/<test_name>.fst` to confirm FST location

#### Step 3b — Interpolate the Waveform
Read the FST file as a text-format signal trace using:
```bash
# Convert FST to VCD then inspect specific time window around the failure
fst2vcd waves/<test_name>.fst 2>/dev/null | \
  awk '/^#[0-9]/{t=$0} /rv_in|rv_out|enqueue|fanout|ingress|bcast|valid|ready/{print t, $0}' | \
  tail -200
```
If `fst2vcd` is unavailable, use:
```bash
python3 -c "
import subprocess, sys
result = subprocess.run(['vcd_info', 'waves/<test_name>.fst'], capture_output=True, text=True)
print(result.stdout[:4000])
"
```
Focus the waveform review on:
- The **10–20 clock cycles before the failure timestamp** to identify setup conditions.
- The **handshake signals** for the failing path: `rv_in_valid`, `rv_in_ready`, `rv_out_valid`, `rv_out_ready`, and any control signals named in the error.
- Any signal that holds `x` or oscillates without settling — a strong indicator of a combinational loop or undriven wire.

#### Step 3c — Cross-Reference Architecture Docs
Identify the RTL module(s) involved in the failure. Map module name to doc file:

| Module | Doc file |
|---|---|
| `tile_top` | `docs/tile_top_architecture.md` |
| `tile_ingress` | `docs/tile_top_architecture.md` §6.1 |
| `tile_dispatch_scheduler` | `docs/tile_top_architecture.md` §6.2 |
| `tile_fanout_executor` | `docs/tile_top_architecture.md` §6.3 |
| `tile_host_io` | `docs/tile_top_architecture.md` §6.4 |
| `neuron_compute_core` | `docs/neuron_compute_core_architecture.md` |
| `neuron_exec` | `docs/neuron_exec_architecture.md` |
| `logical_neuron_context_bank` | `docs/logical_neuron_context_bank_architecture.md` |
| `tile_top_tt` / `tt_um_neutern_0` | `docs/tt_um_neutern_0_architecture.md` |

Read only the sections relevant to the failing signal path. Confirm that the RTL behaviour observed in the waveform is consistent with the documented expected behaviour. If not, the discrepancy is the root cause.

#### Step 3d — Identify Root Cause
From log + waveform + doc evidence, classify the failure:

| Category | Indicators | Typical fix location |
|---|---|---|
| **OOB array index** | `x` on array output, DIDNOTCONVERGE | `tile_top.sv` — add `(N==1) ? 0 :` guard |
| **Combinational loop** | DIDNOTCONVERGE, oscillating signal | Break comb path; separate `always_comb` blocks |
| **Handshake deadlock** | Timeout on `rv_in_ready` or `rv_out_valid` | Check FIFO full/empty gating, backpressure signals |
| **X propagation** | `"x" in str(dut.signal.value)` | Check reset coverage, undriven wire, missing `else` in `always_comb` |
| **Wrong field decode** | Assert on unexpected packet field value | Check bit-slice in `tile_ingress.sv` or `tile_top.sv` against `message_packet_t` layout |
| **Count mismatch** | Assert `len(received) != expected` | Check broadcast serializer counter, FIFO drain wait |
| **Timing / wait too short** | Timeout, insufficient wait cycles | Increase `ClockCycles()` in test **only** if RTL is verified correct |

#### Step 3e — Apply Smallest RTL Patch
- Read the full relevant RTL block (±20 lines around the identified line) before editing.
- Make the **minimum change** that fixes the root cause without altering synthesis behaviour.
- Do not refactor surrounding code, add comments to unchanged lines, or add features not needed for the fix.
- After editing, confirm the change compiles cleanly:
  ```bash
  cd <TEST_DIR> && make SIM=verilator TESTCASE=<test_name> 2>&1 | grep -E "Error|error:|TESTS=" | head -20
  ```

#### Step 3f — Rerun the Fixed Test
```bash
cd <TEST_DIR> && make SIM=verilator TESTCASE=<test_name> 2>&1 | tail -10
```
- If `PASS`: mark the test resolved, move to the next failing test.
- If still `FAIL`: return to Step 3a with the new waveform for the patched design. Do **not** reuse the old waveform.
- Maximum iterations per test: **5**. If a test is still failing after 5 patch attempts, mark it `BLOCKED` with full evidence and continue to the next test.

### Phase 4 — Full Suite Rerun
After all per-test debug loops complete:
```bash
cd <TEST_DIR> && make SIM=verilator 2>&1 | tail -5
```
Parse `TESTS=N PASS=P FAIL=F`.
- If `F == 0` and `P == N`: proceed to Phase 5.
- If any regressions were introduced (a previously passing test now fails), treat them as new failures and re-enter Phase 3 for each.
- Repeat Phase 4 until `P == N`.

### Phase 5 — Final Report
Output a structured summary:

```
## cocotb Debug Session Report

**Test suite:** <TEST_DIR>/<TEST_MODULE>.py
**DUT:** <DUT_NAME>
**Simulator:** verilator
**Total tests:** N  (confirmed from TESTS=N cocotb summary)

### RTL Patches Applied

| # | File | Line(s) | Root cause | Fix description |
|---|---|---|---|---|
| 1 | src/tile/src/tile_top.sv | 560–568 | OOB index on eq_not_full[1] for WORKER_CORES_PER_TILE=1 | Guard enq_worker_sel_c with `(WORKER_CORES_PER_TILE==1) ? 0 : ...` |
| … | … | … | … | … |

### Test Pass Table

| Test name | Status | Failure category | Patch # |
|---|---|---|---|
| test_reset_idle | PASS | — | — |
| test_spike_unicast_each_neuron | PASS | OOB array index | 1 |
| … | … | … | … |

**Final result:** TESTS=N PASS=N FAIL=0 SKIP=0 ✅
```

The total row count in the Test Pass Table **must equal** `TESTS=N`. If any test is `BLOCKED`, include it in the table with status `BLOCKED` and explain why in an `## Open Issues` section below the table.

---

## Waveform Interpolation Guide

### Tool priority (in order)
1. `fst2vcd <file>.fst | grep -A2 -B2 "<signal>"` — fastest, signal-focused
2. `python3 -c "import fst; ..."` — if fst Python bindings are installed
3. Read the raw FST as binary only as a last resort for header metadata

### Signals to always inspect for tile_top failures
```
rv_in_valid    rv_in_ready    rv_in_payload
rv_out_valid   rv_out_ready   rv_out_payload
enqueue_ev_if_valid    enqueue_ev_if_ready
logical_event_full_c   eq_not_full
ingress_bcast_in_progress_r   ingress_bcast_addr_r
fanout_ev_if_valid    fanout_ev_if_ready
enq_worker_sel_c
```

### Time window strategy
- **DIDNOTCONVERGE**: inspect cycles 0–50 (loop activates at first spike after `min_cfg_loaded_r=1`)
- **Handshake timeout**: inspect last 50 cycles before the reported timeout timestamp
- **X propagation**: start from reset deassertion (cycle ~8–10) and scan forward for first `x`
- **Count mismatch**: inspect the full transaction sequence from the first send to the last expected receive

### Reading waveform output
Convert FST signal transitions to a readable clock-aligned table:
```bash
fst2vcd waves/<test>.fst 2>/dev/null | python3 -c "
import sys, re
t = 0
rows = []
for line in sys.stdin:
    line = line.rstrip()
    if line.startswith('#'):
        t = int(line[1:])
    elif line.startswith('b') or line.startswith('0') or line.startswith('1'):
        rows.append((t, line))

# Print last 100 transitions
for ts, val in rows[-100:]:
    print(f't={ts:8d}  {val}')
" 2>/dev/null | head -120
```

---

## Known Testbench Patterns and Fixes (Learned from Sessions)

Apply as first-line checks before waveform analysis when the symptom matches:

1. **`enq_worker_sel_c` OOB for WORKER_CORES_PER_TILE=1**
   - Symptom: DIDNOTCONVERGE at first spike after config. `eq_not_full[1]` resolves to `x`.
   - Root cause: `WORKER_IDX_W'(neuron_idx[WORKER_IDX_W-1:0])` selects bit[0] which alternates 0/1 for even/odd neurons, but only worker 0 exists.
   - Fix: `enq_worker_sel_c = (WORKER_CORES_PER_TILE == 1) ? WORKER_IDX_W'(0) : WORKER_IDX_W'(enqueue_ev_payload_c.neuron_idx[WORKER_IDX_W-1:0]);`
   - File: `src/tile/src/tile_top.sv`

2. **Broadcast serializer drain race (backpressure timeout)**
   - Symptom: `rv_in handshake timeout after 50 cycles` on the unicast spike immediately after a broadcast spike.
   - Root cause: broadcast fills the 4-deep FIFO before the worker drains it; the 20-cycle gap between broadcast and unicasts is insufficient.
   - Fix: increase `ClockCycles(dut.clk, 20)` to ≥60 after the broadcast send, OR fix the underlying FIFO OOB issue (pattern #1) which resolves this as a side-effect.
   - File: `src/tile/test/test_tile_top.py` (test-side fix, only if RTL is confirmed correct)

3. **`$dumpfile` with dynamic path in Verilator**
   - Symptom: FST file not created when `+tracefile=<path>` plusarg is used.
   - Root cause: `$value$plusargs` must read into a `string` or `reg[N:0]` declared at module scope before the `initial` block.
   - Fix: declare `string wave_path;` at module scope; use `initial begin if ($test$plusargs(...)) begin $dumpfile(wave_path); end end`.

4. **Combinational loop through dynamic array index**
   - Symptom: Verilator DIDNOTCONVERGE, oscillating `logical_event_full_c`.
   - Root cause: `eq_not_full[enq_worker_sel_c]` where `enq_worker_sel_c` is derived from `fanout_ev_if.rv_payload`, and `fanout_ev_if.ready = !logical_event_full_c` feeds back into the fanout executor which drives `fanout_ev_if.valid` / `rv_payload`.
   - Fix: any of (a) constant-fold the index for single-worker configs, (b) separate into a dedicated `always_comb` block that reads from registered sources only, (c) add `/* verilator split_var */` to the array declaration.

5. **`min_cfg_loaded_r` gate prevents loop activation in config-only tests**
   - Symptom: tests that only send CSR/ucode/weight packets pass; tests that send spikes fail.
   - Explanation: `tile_ingress` sets `min_cfg_loaded_r=1` after the first `CSR_CTRL` write. The combinational loop in `tile_top.sv` is only active after this flag is set. Config-only tests never trigger the loop.
   - Action: when triaging DIDNOTCONVERGE, check whether the test sends a spike **after** a CSR_CTRL write.

6. **`EVENT_TIME_W=6` field width — `ValueError` on out-of-range `last_time` assignments**
   - Symptom: `ValueError: Value ... requires X bits to represent, but signal ... is only 6 bits wide` on first compile+run.
   - Root cause: `tile_pkg.sv` declares `EVENT_TIME_W = 6` (not 16). Any `last_time` literal larger than 63 is rejected by cocotb 2.0.1 at assignment time.
   - Fix: define `MASK_TIME = (1 << EVENT_TIME_W) - 1` at the top of the test module; apply `& MASK_TIME` to every hardcoded `last_time` value.
   - File: test Python module.

7. **Golden model missing fields in stress tests**
   - Symptom: `AssertionError: ucode_len mismatch` or similar on golden-model verification after a randomised stress run, even when state is correct.
   - Root cause: the golden model init loop only updated the primary state field (e.g., `GOLDEN_STATE[i]`) but not the meta fields (`GOLDEN_ULEN`, `GOLDEN_UPTR`, `GOLDEN_FPTR`, `GOLDEN_FLEN`). The RTL's FULL_ROW write stores `ucode_len=1` as the programmed value; the unsynchronised golden still holds 0.
   - Fix: initialise **all** golden model arrays in the init loop to match exactly what the FULL_ROW write will store.
   - File: test Python module.

8. **`gf180mcu_ocd_sram_lint_stubs.v` missing from VERILOG_SOURCES**
   - Symptom: Verilator elaboration error: `Cannot find ...gf180mcu_ocd_...` or `Unknown module ...` during Phase 1 compile.
   - Root cause: The SRAM hierarchy instantiates GF180 PDK macro cells. Even with `MEM_STYLE=0` (FF-backed), the stub file must be in the source list.
   - Fix: add `$(COMMON_MEM)/gf180mcu_ocd_sram_lint_stubs.v` to `VERILOG_SOURCES` in the Makefile before any SRAM wrapper source.
   - File: `test/Makefile`.

---

## Operating Rules

1. **One test at a time.** Do not attempt to batch-fix multiple tests simultaneously unless the patches are provably independent (same root cause, same file, non-overlapping lines).
2. **Evidence-first.** Every patch must cite: log line, waveform signal, doc section. Do not patch from intuition alone.
3. **Minimum diff.** Change the fewest lines needed. Do not reformat, rename, or refactor.
4. **No synthesis regressions.** Any patch to synthesisable RTL must preserve functional equivalence for the synthesis target. Do not add simulation-only guards (`ifdef SIMULATION`) unless the change genuinely must not be synthesised.
5. **Recompile after every patch.** Confirm zero compile errors before running the test.
6. **Preserve passing tests.** After each patch, confirm that the previously passing tests were not broken before moving to the next failing test.
7. **Keep terminals open.** Do not `exit` or kill terminal sessions; preserve history for user review.
8. **Report all counts.** The final pass table row count must exactly equal the cocotb `TESTS=N` value.
9. **BLOCKED ceiling.** If a single test exceeds 5 patch iterations without passing, record it as BLOCKED with full evidence and continue. Do not loop indefinitely.
10. **No test-only workarounds for RTL bugs.** Increasing timeouts or weakening assertions is only permitted when the RTL has been confirmed correct by waveform analysis and the issue is a test-side race.

---

## Guardrails

- Do not claim `PASS` without the cocotb summary line as evidence.
- Do not skip a failing test in the per-test loop.
- Do not synthesise untested RTL changes — always rerun the affected test after each patch.
- Do not add `$display`, `$monitor`, or `assert` statements to RTL unless they are inside an existing `ifdef FORMAL` or equivalent guard.
- Do not modify `tile_top_flat.sv` or testbench Makefiles as a substitute for fixing RTL.
- Do not modify test assertions to make them less strict unless the spec confirms the looser behaviour is correct.
