# Cold Foot ASIC SoC Summary Instructions

## Purpose

Use this file as the architecture summary for future requests about the Cold Foot SoC.

- Project name: Cold Foot
- Repository: coldfoot_soc
- Domain: Spiking Neural Network (SNN) ASIC/SoC

This document is relationship-first and implementation-oriented. Treat RTL and FuseSoC core files as source of truth when details conflict across older docs.

## Scope and Ground Rules

1. Keep SNN behavior neuron-centric and event-driven.
2. Treat fan-out connectivity as source-of-truth for propagation.
3. Do not move firing decisions into synapse logic.
4. Preserve separation between:
   - neuron runtime state
   - static neuron config/control
   - fan-out connectivity
   - ucode storage
5. When packet/protocol behavior changes, update RTL, tests, runtime, and docs together.

## FuseSoC Dependency Picture

From maintained `*.core` files:

- `coldfoot:asic:coldfoot:0.1.0` depends on:
  - `coldfoot:common:base:0.1.0`
   - `coldfoot:ip:host_gateway:0.1.0`
   - `coldfoot:ip:neural_mesh:0.1.0`
  - `coldfoot:ip:tile:0.1.0`
- `coldfoot:ip:tile:0.1.0` depends on:
  - `coldfoot:common:base:0.1.0`
  - `coldfoot:ip:logical_neuron:0.1.0`
  - `coldfoot:ip:neuron_compute:0.1.0`
- `coldfoot:ip:logical_neuron:0.1.0` and `coldfoot:ip:neuron_compute:0.1.0` both depend on:
  - `coldfoot:common:base:0.1.0`

## Block Responsibilities

### `coldfoot_top`

- Chip-edge wrapper.
- Instantiates `host_gateway` and `neural_mesh`.
- Exposes host packet stream plus UART/JTAG pins.

### `neural_mesh`

- Integrates NoC with one `tile_top` per XY location.
- Owns per-node ingress/egress register slices.
- Connects loader direct programming signals from NoC to all tiles.

### `host_gateway` + `neural_mesh`

- Message-oriented XY NoC routing and host arbitration.
- Loader/control window handling and direct loader programming fanout.
- Collects tile spike and tile host responses into shared host egress.

### `tile_top`

- Single flat tile boundary module (the `tile_top_packed` / `tile_top_packed_core` wrappers were flattened into `tile_top` on 2026-04-21).
- Local message classification and tile-local scheduling.
- Event queue and dispatch coordination for logical neurons.
- Worker scheduling for `WORKER_DIM <= Z_DIM`.
- Uses logical-neuron banks and compute workers for runtime execution.

### `logical_neuron` IP

- Persistent neuron control/context/state/ucode storage primitives.
- Shared tile-local memories used by packed tile core.

### `neuron_compute` IP

- Stateless worker execution engines.
- Execute context snapshots and return updated context plus side effects.

## Parameter Semantics (Important)

- `X_DIM`: mesh X tiles.
- `Y_DIM`: mesh Y tiles.
- `WORKER_DIM`: physical worker cores per tile.

Do not assume one universal default set. Defaults vary by context:

- RTL module defaults (for example `coldfoot_top`) can differ from
  FuseSoC target defaults.
- Readme examples may reflect maintained regression profiles, not elaboration defaults.
- FPGA full-core defaults are flow-specific.

Always verify active values from the exact command/target being run.

## Message/Programming Model Snapshot

- Host-facing control path is packetized ready/valid traffic.
- Runtime graph deployment is loader-centric (bundle stream -> NoC loader -> tile programming internals).
- Direct message-level programming is retained mostly for debug/bring-up compatibility and may be intentionally rejected on maintained tile paths.
- Tile/runtime readback and telemetry share the same host egress stream.

## Practical Edit Guidance for Future Requests

When asked to change behavior, map requests to these ownership boundaries:

1. Chip-edge protocol, local ping/hwinfo, broadcast expansion:
   - edit modules under `hw/ip/host_gateway/src/`
2. Mesh-level routing, host arbitration, loader transport:
   - edit `hw/ip/host_gateway/src/*.sv` and `hw/ip/neural_mesh/src/*.sv`
3. Tile-local scheduling, logical-neuron dispatch, fanout-based emission:
   - edit `hw/ip/tile/src/tile_top*.sv` and tile helper banks
4. Persistent logical-neuron storage formats:
   - edit `hw/ip/logical_neuron/src/*.sv`
5. Worker execution semantics:
   - edit `hw/ip/neuron_compute/src/*.sv`
6. SoC integration wiring:
   - edit `asic/src/chip_top.sv` and `asic/src/chip_core_tile.sv`

If packet fields, loader semantics, or scheduling behavior changes, also update:

- SoC and IP testbenches/cocotb coverage
- runtime bundle/compiler assumptions
- relevant README and instruction files

## Verification Anchors

Use these as primary maintained checks:

```sh
fusesoc run --target lint_coldfoot coldfoot:asic:coldfoot:0.1.0
fusesoc run --target sim_coldfoot_local_verilator coldfoot:asic:coldfoot:0.1.0
fusesoc run --target sim_coldfoot coldfoot:asic:coldfoot:0.1.0
python3 tools/flows/tool_flow.py formal --project tile
fusesoc run --target formal coldfoot:asic:coldfoot:0.1.0
```

When changing tile-local scheduling/dispatch, include tile formal and SoC cocotb reruns.

## Source Anchors

- `asic/src/chip_top.sv`
- `asic/src/chip_core_tile.sv`
- `hw/ip/host_gateway/src/host_gateway_top.sv`
- `hw/ip/host_gateway/host_gateway.core`
- `hw/ip/neural_mesh/src/mesh_router.sv`
- `hw/ip/neural_mesh/neural_mesh.core`
- `hw/ip/tile/src/tile_top.sv`
- `hw/ip/tile/tile.core`
- `hw/ip/logical_neuron/logical_neuron.core`
- `hw/ip/neuron_compute/neuron_compute.core`
- `asic/README.md`
- `hw/ip/host_gateway/README.md`
- `hw/ip/neural_mesh/README.md`
- `hw/ip/tile/README.md`
- `hw/ip/logical_neuron/README.md`
- `hw/ip/neuron_compute/README.md`
