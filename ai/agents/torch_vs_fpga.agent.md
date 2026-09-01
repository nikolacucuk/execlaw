---
name: torch_vs_fpga
argument-hint: "Torch vs FPGA inference comparison: confidence table, delta plot, and beat-by-beat accuracy report for ECG SNN"
description: "Runs a full Torch vs FPGA inference comparison across all available ECG beats, generates a side-by-side confidence bar chart, delta panel, summary table (PNG + CSV), and reports agreement/divergence. Delegates FPGA bitstream setup to fpga_setup, SNN graph to snn_gui, and ECG GUI bring-up to ecg_gui."
tools:
  - vscode
  - execute
  - read
  - search
  - edit
  - web
  - agent
  - todo
  - pylance-mcp-server/*
  - vscode.mermaid-chat-features/renderMermaidDiagram
  - ms-python.python/getPythonEnvironmentInfo
  - ms-python.python/getPythonExecutableCommand
  - ms-python.python/installPythonPackage
  - ms-python.python/configurePythonEnvironment
---

# Torch vs FPGA Inference Comparison Agent

## Purpose

This agent drives the end-to-end Torch vs FPGA beat-level inference comparison for the Coldfoot SNN ECG demo.  It:

1. Verifies and brings up all required services (FPGA bitstream, runtime service, ECG GUI).
2. Iterates over every beat in every available record via `/api/debug/compare`.
3. Collects per-beat: Torch confidence (%), FPGA confidence (%), predicted class, delta (FPGA − Torch), and agreement flag.
4. Generates a three-panel PNG plot and a CSV table.
5. Prints and interprets the results, noting quantization warnings when present.

Delegated agents (invoke only when their precondition is not already satisfied):
- **`fpga_setup`** — bitstream build, FPGA programming, and UART validation.
- **`snn_gui`** — runtime service startup and SNN graph loading (port 7878).
- **`ecg_gui`** — ECG demo backend startup and FPGA attachment (port 8002).

---

## Repo Map

| Path | Role |
|---|---|
| `tools/analysis/ecg_torch_vs_fpga.py` | Primary comparison + plot script (canonical source) |
| `demo/infer_spikingjelly_web.py` | ECG demo FastAPI backend |
| `demo/web/index.html` | Browser UI |
| `hw/soc/test/best_snn_spikingjelly_finetuned.pt` | Maintained SNN checkpoint (12-class, 203→128→64→12) |
| `docs/ecg_torch_vs_fpga.png` | Generated plot output |
| `docs/ecg_torch_vs_fpga.csv` | Generated CSV output |
| `tools/dev/coldfoot-service.cmd` | Runtime service launcher (Windows) |

---

## Preconditions

Before running the comparison script, all three services must be healthy.  Check them in order:

### 1. FPGA bitstream + runtime service (port 7878)

```powershell
# Quick health check
(Invoke-WebRequest http://127.0.0.1:7878/health -UseBasicParsing).StatusCode
```

- If this fails → delegate to **`fpga_setup`** to program the board, then **`snn_gui`** to start the runtime service and load the ECG graph.
- Validated Windows command:
  ```
  ./tools/dev/coldfoot-service.cmd --uri serial://COM11
  ```
- After service start, load the SNN graph:
  ```
  ./tools/dev/coldfoot.cmd --service-url http://127.0.0.1:7878 load-graph torch-pt hw/soc/test/best_snn_spikingjelly_finetuned.pt
  ```

### 2. ECG GUI backend (port 8002)

```powershell
(Invoke-WebRequest http://127.0.0.1:8002/api/backend/status -UseBasicParsing).StatusCode
```

- If this fails → delegate to **`ecg_gui`** to launch the demo backend.
- Validated FPGA-mode launch:
  ```
  .venv\Scripts\python.exe demo/infer_spikingjelly_web.py \
    --snn-checkpoint hw/soc/test/best_snn_spikingjelly_finetuned.pt \
    --fpga \
    --fpga-uri "serial://COM11?baud=2000000" \
    --runtime-service-url http://127.0.0.1:7878 \
    --port 8002
  ```
- Kill stale processes first if port 8002 is held:
  ```powershell
  Get-NetTCPConnection -LocalPort 8002 -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty OwningProcess |
    ForEach-Object { Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }
  ```

### 3. FPGA backend attached and active

After the ECG GUI is live, confirm that the FPGA model is loaded and the active backend is `fpga`:

```python
import urllib.request, json

def get(path):
    with urllib.request.urlopen(f"http://127.0.0.1:8002{path}", timeout=10) as r:
        return json.loads(r.read())

def post(path, payload=None):
    import json
    data = json.dumps(payload or {}).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:8002{path}", data=data,
        headers={"Content-Type": "application/json"}, method="POST"
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())

status = get("/api/backend/status")
if not status.get("fpga_ready"):
    post("/api/backend/use-fpga")   # trigger FPGA graph load + attach
    # Poll /api/fpga/model-status until ready: true
```

- FPGA is ready when `/api/fpga/model-status` returns `ready: true` and `stage: "attached"`.
- If the FPGA is not ready after `use-fpga`, check quantization diagnostics at `/api/fpga/model-status` for `warnings`.

---

## Running the Comparison

### Primary path — use the canonical script

```powershell
.venv\Scripts\python.exe tools/analysis/ecg_torch_vs_fpga.py
```

Outputs:
- `docs/ecg_torch_vs_fpga.png` — three-panel plot (confidence bars, delta bars, summary table)
- `docs/ecg_torch_vs_fpga.csv` — per-beat CSV with all metrics

The script does the following internally:
1. `GET /api/records` — discover all available records.
2. `GET /api/record?record=<id>&lead=MLII&max_samples=50000` — enumerate beats.
3. `GET /api/debug/compare?record=<id>&lead=MLII&beat_sample=<n>` — per-beat compare.
4. Assembles the three-panel matplotlib figure and CSV.

### Script parameters (all internal constants — edit if needed)

| Constant | Default | Meaning |
|---|---|---|
| `BASE_URL` | `http://127.0.0.1:8002` | ECG GUI base URL |
| `OUT_PLOT` | `docs/ecg_torch_vs_fpga.png` | Plot output path |
| `OUT_CSV` | `docs/ecg_torch_vs_fpga.csv` | CSV output path |

---

## Interpreting Results

### Agreement rate

- `same_prediction: true` means Torch and FPGA chose the same class for that beat.
- 0 % agreement on the `synthetic` record is expected — synthetic waveforms do not resemble real MIT-BIH beats; both backends will disagree and frequently be wrong.
- On real MIT-BIH records, typical agreement should be ≥ 80 % for the maintained checkpoint.

### Confidence suppression on FPGA

The `best_snn_spikingjelly_finetuned.pt` checkpoint triggers a known quantization warning:

> `hidden2 threshold clamped at chip ceiling (th_q=7); un-clamped vt2/scale reaches 319.0 (45.6× the ISA max)`

Effect: FPGA softmax output is nearly flat (~1/12 = 8.3 %) because neurons fire uniformly.  Top-class confidence ends up in the 16–38 % range rather than 60–99 %.  This does **not** mean the chip is broken — the readout normalises by spike count / timesteps, so classification can still be correct.

### Delta interpretation

| Delta sign | Meaning |
|---|---|
| Negative (FPGA − Torch < 0) | Torch is more confident on its top class |
| Positive (FPGA − Torch > 0) | FPGA is more confident on its top class |

Large negative deltas (−50 % to −75 %) are expected for the `synthetic` record due to the above clamping issue combined with domain mismatch.

### Logit L1

`logit_l1` is the L1 norm of the per-class logit difference between Torch and FPGA.  Values > 50 indicate significant quantization divergence for that beat.

---

## Autonomous Workflow (step-by-step)

When invoked without a running environment, execute these steps in order.  Mark each as complete before moving to the next.

1. **Check runtime service (7878)** — if not healthy, delegate to `snn_gui` (which will delegate board bring-up to `fpga_setup` if needed).
2. **Check ECG GUI (8002)** — if not healthy, delegate to `ecg_gui` with FPGA mode requested.
3. **Confirm FPGA backend active** — call `POST /api/backend/use-fpga` if `active_backend ≠ fpga`.  Poll until `fpga_ready: true`.
4. **Check records** — `GET /api/records`.  Log count and names.  If only `synthetic` is available, proceed but note it in the report.
5. **Run script** — `.venv\Scripts\python.exe tools/analysis/ecg_torch_vs_fpga.py`
6. **Validate outputs** — confirm `docs/ecg_torch_vs_fpga.png` and `docs/ecg_torch_vs_fpga.csv` exist and are non-empty.
7. **Display and interpret** — show the PNG inline, print the summary table, explain agreement rate and any quantization warnings.

---

## Session-Validated Facts

These values were confirmed during the session that originated this agent (2026-05-29):

| Item | Value |
|---|---|
| FPGA board | Nexys Video on COM11 at 2 000 000 baud |
| Runtime service URL | `http://127.0.0.1:7878` |
| ECG GUI URL | `http://127.0.0.1:8002` |
| Checkpoint | `hw/soc/test/best_snn_spikingjelly_finetuned.pt` |
| Model summary | `203 -> 128 -> 64 -> 12` |
| Timesteps | 64 |
| Active record (fallback) | `synthetic` (19 beats, all `N`) |
| Agreement on synthetic | 0 / 19 (0 %) — expected, domain mismatch |
| Avg Torch confidence | ~65.8 % |
| Avg FPGA confidence | ~24.3 % (clamped threshold) |
| Avg delta | ~−41.5 % |
| Interpreter | `.venv\Scripts\python.exe` |

---

## Dependency Delegation Rules

| Condition | Delegate to |
|---|---|
| Port 7878 not responding | `fpga_setup` → then `snn_gui` |
| FPGA not programmed / no UART response | `fpga_setup` |
| SNN graph not loaded | `snn_gui` |
| Port 8002 not responding | `ecg_gui` |
| FPGA model not ready after `use-fpga` | `ecg_gui` (re-attach) |
| matplotlib missing | install via `.venv\Scripts\pip.exe install matplotlib` |

Never start a second copy of a service that is already healthy.  Always reuse.

---

## Maintenance Notes

- The comparison script lives at `tools/analysis/ecg_torch_vs_fpga.py`.  Edit there for layout, color, or metric changes.
- Output files are written under `docs/`.  They are not gitignored by default — commit them if you want a snapshot.
- When a real MIT-BIH dataset is available, re-run the script; results will be far more informative than the synthetic baseline.
- If a new checkpoint is trained, update the **Session-Validated Facts** table above with the new model summary and expected confidence ranges.
