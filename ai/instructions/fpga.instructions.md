# FPGA Development Instructions: Nexys Video + Vivado

## Scope

Nexys Video FPGA bring-up and iterative development for the coord-addressable
ColdFoot SoC.  All flow is FuseSoC-native; Vivado runs in batch mode under
edalize.

- ASIC / GF180MCU flow lives in `ai/instructions/gf180mcu.instructions.md`.
- Wafer.space MPW logistics live in `ai/instructions/wafer.space.instructions.md`.
- This file covers Nexys Video hardware, FuseSoC targets, bitstream
  management, UART validation, and common gotchas.

## Hardware Platform Snapshot

| Aspect            | Value                                                         |
|-------------------|---------------------------------------------------------------|
| Board             | Digilent Nexys Video                                          |
| FPGA              | Xilinx Artix-7 `xc7a200tsbg484-1`                             |
| Flash             | Spansion S25FL256S (256 Mbit QSPI) for persistent boot        |
| USB-JTAG / UART   | FT2232HQ — channel A = JTAG, channel B = UART                 |
| Default mesh      | **1×1** (single tile + host_shim).  Scale via `MESH_X / MESH_Y`. |
| Default sys clock | 40 MHz (MMCM from 100 MHz `clk100`)                           |
| Default UART baud | 2 000 000                                                     |
| OLED              | SSD1306 128×32 (optional, gated by `ENABLE_OLED_STATUS`)      |
| LEDs              | 8-bit telemetry indicators driven from soc_top output flatten |

Reference build (1×1 mesh @ 40 MHz, May 2026):
- Full Vivado flow time: ~5 min on a modern desktop (synth ~1m20, opt+place+route ~2m, bitgen ~1m).
- Bitstream size: ~9.3 MB.

## FuseSoC Targets

All commands run from the repo root.

```sh
# Verilator lint over the full hierarchy.  0 errors, 0 warnings at HEAD.
fusesoc run --target lint      coldfoot:fpga:nexys_video

# Vivado synth-only.  Catches synth-specific elaboration errors without
# paying the place / route / bitgen cost.
fusesoc run --target synth     coldfoot:fpga:nexys_video

# Vivado full flow: synth -> opt -> place -> route -> write_bitstream.
fusesoc run --target bitstream coldfoot:fpga:nexys_video

# JTAG-load the bitstream onto the FPGA (volatile, lost on power cycle).
fusesoc run --target program   coldfoot:fpga:nexys_video

# Write the bitstream to the QSPI configuration flash (persistent).
fusesoc run --target flash     coldfoot:fpga:nexys_video
```

`program` / `flash` are driven by `tools/flows/fpga_program.py`, which:
- auto-discovers the canonical `.bit` produced by the `bitstream` target,
- writes a TCL file on the fly,
- invokes `vivado -mode batch -source <tcl>`.

The wrapper assumes a Vivado install reachable via `$VIVADO` (env var) or `PATH`.

> **Windows PowerShell**: `vivado` is typically **not** on `PATH` by default.
> You must set `$env:VIVADO` before calling `fpga_program.py` directly:
> ```powershell
> $env:VIVADO = "C:\AMDDesignTools\2025.2\Vivado\bin\vivado.bat"
> .venv\Scripts\python.exe tools\flows\fpga_program.py --mode program `
>     --bitstream build-fusesoc\coldfoot_fpga_nexys_video_0.1.0\bitstream-vivado\coldfoot_fpga_nexys_video_0.1.0.bit
> ```
> The `--mode` flag (`program` or `flash`) is required.  If you use
> `fusesoc run --target program`, FuseSoC sets `$VIVADO` and the mode for you.

## Parameter Overrides

Append `--<NAME> <value>` to any FuseSoC command.

| Vlogparam                          | Default     | Notes                                       |
|------------------------------------|-------------|---------------------------------------------|
| `MESH_X`                           | 1           | Mesh X dim.  Grows logic ~linearly.         |
| `MESH_Y`                           | 1           | Mesh Y dim.                                 |
| `MESH_Z`                           | 255         | Broadcast core-id sentinel.                 |
| `SYS_CLK_HZ`                       | 40 000 000  | MMCM target.  Lower if timing fails.        |
| `UART_BAUD`                        | 2 000 000   | Matches the runtime CLI's default baud.     |
| `ENABLE_OLED_STATUS`               | false       | Drive the SSD1306 OLED with the boot screen.|
| `COLDFOOT_TILE_BANK_MEM_STYLE`     | 2           | 2 = Xilinx `xpm_memory_sdpram` (BRAM).      |
| `COLDFOOT_MAX_SYNAPSE_SRAM_DEPTH`  | 4096        | Must be ≥ tile_top.SYNAPSE_SRAM_DEPTH.      |

Example: 2×2 grid at 50 MHz:

```sh
fusesoc run --target bitstream coldfoot:fpga:nexys_video \
    --MESH_X 2 --MESH_Y 2 --SYS_CLK_HZ 50000000
```

## On-Board Bring-Up

### 1. Build the bitstream

```sh
fusesoc run --target bitstream coldfoot:fpga:nexys_video
```

Output: `build-fusesoc/coldfoot_fpga_nexys_video_0.1.0/bitstream-vivado/coldfoot_fpga_nexys_video_0.1.0.bit`.

### 2. JTAG-program the FPGA

```sh
fusesoc run --target program coldfoot:fpga:nexys_video
```

Expected output: `Opening hw_target localhost:3121/xilinx_tcf/Digilent/<serial>`,
`End of startup status: HIGH`, `vivado returned 0`.

### 3. Bind the FT2232 channel B UART (Windows-only one-time setup)

Out of the box, Windows lets Vivado claim the FT2232 channel A as JTAG via
the FTDI D2XX driver — but channel B (the UART) often has no VCP driver
bound and no COM port appears.

Fix:
1. Open Device Manager.
2. Expand **Universal Serial Bus controllers**.
3. Find the FT2232's interface 1 (the second "USB Serial Converter B" entry
   matching the Nexys Video serial).  Right-click → Properties.
4. **Advanced** tab → check **Load VCP**.
5. Unplug and replug the board.
6. A new COM port (e.g. `COM7`) should now appear under **Ports (COM & LPT)**.

Alternative: use the FTDI **FT_PROG** utility to enable VCP on channel B in
the chip's EEPROM (persists across power cycles).

### 4. Send a ping over UART

```sh
sw/runtime/target/debug/coldfoot --port COM<N> --direct --timeout 3 ping
```

Successful round-trip exits zero and prints the PONG decode.  A timeout
points back to one of:
- Wrong COM port (verify in Device Manager).
- VCP not bound on channel B (step 3 above).
- Wrong baud (the runtime defaults to 2 000 000 — matches the FPGA default).
- FPGA not actually programmed (re-run target program).

## UART Protocol Reference

The SoC speaks **binary framed messages** over UART, not ASCII.

- Frame: `[0xA5 SOF] [11 bytes big-endian payload]` (12 bytes total).
- Default baud: 2 000 000 (one bit per 20 sys-clock cycles at 40 MHz).
- Message kinds (subset; see `hw/common/packages/tile_pkg.sv` and
  `sw/runtime/python/coldfoot_runtime/protocol.py` for the authoritative list):

| Kind       | Value | Direction      | Notes                                       |
|------------|-------|----------------|---------------------------------------------|
| MSG_WRITE  | 0     | host → tile    | Loader / CSR writes.                        |
| MSG_READ   | 1     | host → tile    | CSR / SRAM reads.                           |
| MSG_READ_RSP | 2   | tile → host    | Read response.                              |
| MSG_OUTPUT | 7     | tile → host    | Spike output bound for host.                |
| MSG_PING   | 8     | host → tile    | Ping request.                               |
| MSG_PONG   | 9     | tile → host    | Pong response.                              |
| MSG_INPUT  | 10    | host → tile    | Spike input.                                |
| MSG_SPIKE  | 11    | tile → tile    | NoC spike packet (not host-bound).          |
| MSG_MCAST  | 14    | mesh → tiles   | Multicast group expansion.                  |
| MSG_ROUTE_PROGRAM | 15 | host → mesh | One-shot router coord seed (Phase 1.5).     |

Addressing: host coord is `(HOST_X, HOST_Y) = (0, 1)`.  Tiles live at
`(1..X_DIM, 1..Y_DIM)`.  `(0, 0)` is unaddressable.

## Troubleshooting

### `vivado` not found / `make` not found on Windows

FuseSoC uses edalize to generate a `Makefile` and calls `make`.  On Windows
without `make` the `bitstream` target fails after generating the build dir —
but the TCL files are already written.  Two-step workaround:

```powershell
# 1. Let FuseSoC generate TCL (fails at make-invocation — that is expected)
fusesoc run --target bitstream coldfoot:fpga:nexys_video

# 2. cd INTO the build dir in the SAME terminal session
$builddir = "$PWD\build-fusesoc\coldfoot_fpga_nexys_video_0.1.0\bitstream-vivado"
Set-Location $builddir

# 3. Create the Vivado project (step A)
& "C:\AMDDesignTools\2025.2\Vivado\bin\vivado.bat" -notrace -mode batch `
    -source coldfoot_fpga_nexys_video_0.1.0.tcl
# Verify: coldfoot_fpga_nexys_video_0.1.0.xpr must exist before step B

# 4. Run synthesis + implementation (step B)
#    MUST be run from inside $builddir; use a relative log path
& "C:\AMDDesignTools\2025.2\Vivado\bin\vivado.bat" -notrace -mode batch `
    -source coldfoot_fpga_nexys_video_0.1.0_synth.tcl `
    -source coldfoot_fpga_nexys_video_0.1.0_run.tcl `
    coldfoot_fpga_nexys_video_0.1.0.xpr `
    2>&1 | Tee-Object -FilePath .\vivado_build.log

# 5. Return to repo root
Set-Location ..\..\..
```

**Critical rules**:
- Steps 3 and 4 must run in the **same terminal session** after `Set-Location`.
  Switching terminals loses `$builddir` and `Tee-Object -FilePath "$builddir\..."` writes to `C:\` (access denied).
- Use `.\vivado_build.log` (relative), not `"$builddir\vivado_build.log"` (absolute via variable), for the log path.
- Install `make` to avoid the workaround: `winget install GnuWin32.Make` or use a Git-for-Windows / MSYS2 shell.

### Lint passes but synth fails

Likely culprit: a real RTL bug or strict-Vivado-only construct.  Examples
that have actually bitten us:

- `input logic` on module ports under `` `default_nettype none `` (Vivado
  wants explicit `input wire` / `input var`).
- Two `.sv` files defining the same module name (verilator tolerates with
  `-Wno-MODDUP`; Vivado does not).
- Elaboration-time `$fatal` triggered by a parameter mismatch (e.g.
  `SYN_ADDR_W > FANOUT_PTR_W` when `COLDFOOT_MAX_SYNAPSE_SRAM_DEPTH` isn't
  large enough).

Read the `synth_1/runme.log` from the bitstream build for the first ERROR
line — that's the root cause; later errors usually cascade.

### Timing fails (place / route)

- Lower `SYS_CLK_HZ` (try 30 MHz).
- Reduce `MESH_X` / `MESH_Y` if scaling.
- Check the routed timing report under
  `build-fusesoc/coldfoot_fpga_nexys_video_0.1.0/bitstream-vivado/coldfoot_fpga_nexys_video_0.1.0.runs/impl_1/`.

### Program succeeds but UART times out

- Most common: FT2232 channel B VCP not bound (see step 3 above).
- Verify the COM port actually corresponds to the Nexys Video, not a
  different USB-serial cable.  Cross-check the FTDI serial number in
  Device Manager against the JTAG serial Vivado prints during programming.
- Verify baud matches: the runtime defaults to 2 000 000; the FPGA build
  also defaults to 2 000 000.  If you overrode `UART_BAUD` at build time,
  pass the same `--baud` to the runtime.

### Vivado `[Labtoolstcl 44-503] No writable temporary directory`

Run from a writable directory.  PowerShell sandboxing and Git Bash's
`/tmp` aliasing have both tripped this — `cd` to the repo root or
`$HOME\Documents` and retry.

### Vivado `[Labtools 27-3733] Error during cs_server initialization`

The TCL is passing an explicit `-cs_url` that points to a port `cs_server`
isn't running on.  `fpga_program.py` omits `-cs_url` so Vivado auto-launches
cs_server; if you've forked the wrapper, drop the hard-coded port.

## File Inventory

- `fpga/nexys_video/coldfoot_fpga.core` — FuseSoC core (5 targets).
- `fpga/nexys_video/rtl/nexys_video_top.sv` — board synthesis root.
- `fpga/nexys_video/rtl/nexys_video_oled_status.sv` — optional OLED renderer.
- `fpga/nexys_video/rtl/gf180_macro_stubs.sv` — empty stubs for GF180 OCD
  SRAM macros (lint-only; dead-branch generate-ifs in `tile_mem_1r1w_sync`).
- `fpga/nexys_video/rtl/xilinx_primitive_stubs.sv` — lint-only stubs for
  `MMCME2_BASE` / `BUFG`.  Synth uses real unisim.
- `fpga/nexys_video/constraints/nexys_video_base.xdc` — board pin / clock
  constraints.
- `fpga/nexys_video/scripts/create_project.tcl` — Vivado GUI project
  generator (driven by `tools/dev/vivado_gui.py`; separate from FuseSoC).
- `tools/flows/fpga_program.py` — TCL-generating wrapper for the `program`
  and `flash` script hooks.
- `hw/soc/src/soc_top.sv` — deployment-agnostic SoC integrator (`X_DIM ×
  Y_DIM` grid + one `host_shim`).

## SNN Inference Integration

After a successful `program` run, use the Rust runtime service and CLI for
graph loading and inference.

### Runtime service

```powershell
# Start the service (keeps the COM port; exposes REST + WebSocket on port 7878)
.\tools\dev\coldfoot-service.cmd --uri serial://COM11
# Health check: GET http://127.0.0.1:7878/health → 200
```

Default service port: **7878** (hardcoded in `coldfoot-runtime-core/src/lib.rs`).

### Graph loading

```powershell
# Load the validated 12-class ECG model (192 neurons, 8087 edges, 27401 words)
.\tools\dev\coldfoot.cmd --service-url http://127.0.0.1:7878 `
    load-graph torch-pt hw/soc/test/best_snn_spikingjelly_finetuned.pt
# → {"loaded":true,"ok":true,"graph":{"nodes":192,"edges":8087},...}

# Or load a small sample graph for bring-up
.\tools\dev\coldfoot.cmd --service-url http://127.0.0.1:7878 `
    load-graph sample --inputs 1 --outputs 1 --hidden-layers 2 --hidden-nodes 4
```

Validated model (`hw/soc/test/best_snn_spikingjelly_finetuned.pt`):
- Architecture: 203 → 128 → 64 → 12 (fc1/fc2 on mesh, fc3 host-side)
- Neuron type: LIF, timesteps: 64
- Labels: `/`, `A`, `E`, `F`, `J`, `L`, `N`, `R`, `V`, `a`, `f`, `j`
- Fits in a **1×1 mesh** (z_dim=255 ≥ 192 neurons needed)

### ECG demo

```powershell
# Kill any stale process on port 8002 first
Get-NetTCPConnection -LocalPort 8002 -ErrorAction SilentlyContinue | `
    Select-Object -ExpandProperty OwningProcess | `
    ForEach-Object { Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }

# Launch with FPGA backend (use --fpga-uri, NOT --fpga-port)
.venv\Scripts\python.exe demo/infer_spikingjelly_web.py `
    --snn-checkpoint hw/soc/test/best_snn_spikingjelly_finetuned.pt `
    --fpga `
    --fpga-uri "serial://COM11?baud=2000000" `
    --runtime-service-url http://127.0.0.1:7878 `
    --port 8002
# Open http://127.0.0.1:8002
```

> **`--fpga-uri` not `--fpga-port`**: the demo CLI argument is `--fpga-uri`.
> Using `--fpga-port` fails silently with "unrecognized arguments".

## Update Policy

- After board / device changes: refresh the Hardware Platform Snapshot.
- After flow changes: re-verify the FuseSoC commands run clean.
- After UART protocol changes: update the kind table and runtime cross-ref.
- Keep this file focused on Nexys Video / FPGA.  Process and MPW questions
  belong in their own files.
