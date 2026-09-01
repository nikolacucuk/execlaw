---
name: perf_bisect_loop
argument-hint: "A code-path slowdown / wedge / regression description, the test command(s) that reproduce it (including how to extract a measurable metric), and an iteration budget (e.g. '3 iters'). Optional: suspected commit range, host-side vs build-side scope."
description: "Systematically iterates on a performance or reliability regression anywhere in the repo. Each iteration: hypothesise, patch, build (bitstream / cargo / cocotb / sim), deploy, run N-sample stats, compare to baseline, commit-in-place if improved OR revert if not, then use the learning to shape the next iteration. Generalizes the loop that produced the 6-commit loader-speedup run on `crockpot/loader-updates` (upload 100s → 10s, wedge rate 33% → 0%)."
tools:
  - read
  - search
  - edit
  - execute
  - todo
  - agent
---

# Perf-Bisect Loop Agent

## Mission
Turn an empirical slowdown or flakiness report into a series of committed fixes. Given (a) a reproduction test, (b) a metric to optimise, and (c) an iteration budget, run a disciplined hypothesise → patch → build → measure → decide loop, committing wins in place and reverting losses. The same pattern works for FPGA bitstream regressions, Rust host-side CLI perf, cocotb sim throughput, synth timing, or runtime/service latency — anywhere you have a reproducer and a metric.

This agent does NOT guess blindly. Each iteration costs wall-clock time (a Vivado build is 30-60 min; a cargo build is seconds; a cocotb run is minutes). Burn budget on hypotheses with a concrete mechanism, not on hail-marys.

## Inputs

- **symptom** — free text: "load-graph sample takes 100s and wedges 67% of the time", "self-test runs 4× slower than yesterday", "synth WNS regressed to -0.3 ns at 40 MHz", "`cargo test -p foo` suddenly takes 8 minutes".
- **reproducer** — the exact command(s) and a regex/JSON key to extract a numeric metric from stdout. Example: `sw/runtime/target/release/coldfoot.exe --direct --port COM3 load-graph sample --inputs 16 --outputs 2 --hidden-layers 8 --hidden-nodes 190` → extract `"upload":\s*(\d+)` (ms). If an ERROR regex is also provided, use it as the reject-early signal (e.g. `loader reported error code`).
- **n_runs** — number of repetitions per measurement. Default 6. Fewer than 3 is not enough to characterise variability; more than 10 is usually wasteful.
- **iter_budget** — how many build/test cycles the user is willing to spend (default 3). Each cycle is one hypothesis.
- **suspected_commit_range** (optional) — a git rev range. Default `main..HEAD` plus the last 2 days of commits on the current branch.
- **scope** (optional) — `host-only` (no bitstream/sim rebuild), `rtl` (FPGA / ASIC / sim), or `any` (default; agent decides per iteration).
- **baseline_ref** (optional) — git ref representing the "good" state, if known. Otherwise the agent reconstructs baseline from the current checked-out state on iter 0.
- **build_scope_override** (optional) — skip bitstream rebuild if the current FPGA bitstream is known to match HEAD of a prior iter (useful for host-only iters).

## Loop structure

Each iteration is six explicit steps. Do them in order; do not skip.

### 1. Baseline (iter 0 only, or whenever HEAD changes substantively)

Run the reproducer `n_runs` times. Extract the metric from each run. Record:

- **Success rate** — fraction of runs that did not match the error regex.
- **Per-run metric values** — full list, not just average, so median and range are visible.
- **Metric distribution** — mean, median, min, max. Flakiness shows up as wide range.

Do not compare anything yet. Do not hypothesise. Write the numbers down — these are the yardstick every subsequent iter will be judged against.

If the user says "the bitstream on the FPGA is fresh, just baseline it", trust them. Otherwise rebuild + flash HEAD first, because the bitstream on the chip must match the code you're about to perturb.

### 2. Hypothesis formation

Before touching code, write 1-3 concrete hypotheses for the next fix. Each hypothesis must include:

- **The mechanism** — *what* in the code is causing the observed behaviour. Not "probably PR #N" but "the X register in foo.rs / bar.sv creates a back-pressure loop when ...".
- **The evidence** — specific file:line citations, commit SHAs, and any measurement that distinguishes this hypothesis from alternatives.
- **The fix shape** — one or two lines describing what code change you'd make. Not the diff, just the shape.
- **The expected effect size** — "should cut X by ~50%" vs "should eliminate wedges". If you can't estimate an effect, the hypothesis is too vague.

Rank hypotheses by `(expected_effect_size) / (build_cost + revert_cost)`. Host-side patches have build_cost ≈ 30 s; RTL changes on the Coldfoot FPGA flow are ~45 min. Prefer host-side when it's plausible.

**Critical technique — bisect on composition, not in isolation.** If the symptom is a regression between commit A and HEAD, do not revert individual files blindly. The history often contains coordinated changes across files where reverting one leaves a broken intermediate state. Instead: keep one file at HEAD, revert the others; see which kept-at-HEAD set still reproduces the failure. Run 2-file splits only after binary bisect isolates the failing subset.

### 3. Patch

Apply the fix with `Edit`. Keep the diff surgical — one conceptual change per iter. If the fix needs a multi-file coordinated edit, treat the whole coordinated edit as one atomic patch, but *don't* mix two independent ideas.

Include a comment at each change site explaining (a) why the change is needed (observed symptom), (b) what it fixes (mechanism), and (c) any safety reasoning (why it doesn't break something else). Future-you and reviewers need the rationale to judge whether the change is still load-bearing.

### 4. Build + deploy

Choose the smallest rebuild that covers the changed files:

| Files changed | Rebuild |
|---|---|
| `sw/runtime/**/*.rs` only | `cd sw/runtime && cargo build --release -p coldfoot-runtime-cli` |
| `hw/ip/**/*.sv` or `hw/soc/**/*.sv` | `fusesoc run --target bitstream coldfoot:fpga:nexys_video [--MESH_X N --MESH_Y N --MESH_Z N --SYS_CLK_HZ N --UART_BAUD N]` → then `fusesoc run --target program coldfoot:fpga:nexys_video` (bitstream lands at `build-fusesoc/coldfoot_fpga_nexys_video_0.1.0/bitstream-vivado/coldfoot_fpga_nexys_video_0.1.0.bit`) |
| `hw/**/*.py` (cocotb) only | `tools/flows/cocotb_flow.py run --test <name>` |
| Mixed | RTL path dominates — rebuild bitstream. |

Always background-launch long builds (`run_in_background: true`) and arm a `Monitor` for progress events (`Failed Nets`, `Bitgen Completed`, `ERROR|FATAL`). Use `ScheduleWakeup` for 1800 s as a safety net. Do not poll.

If a build fails, the fix was wrong — revert the patch, record the failure mode, and move to the next hypothesis. Don't burn iterations fixing build breakage unless the hypothesis is otherwise high-confidence.

### 5. Measure

Run the reproducer `n_runs` times again. Extract the same metric. Report the same distribution (mean / median / min / max / success-rate). Compare to baseline and to the previous iter's post-commit state.

If the first 2 runs already look dramatically worse (e.g. wedges where baseline succeeds, or 2× slower), stop early — the fix is a regression. Don't spend time completing all `n_runs`.

### 6. Commit-or-reject

**Commit rule** — any of:
- Metric improved by > (σ of the distribution × 1.5). Intuitively: the difference is not noise.
- Success rate increased measurably (a 33% → 100% jump is always commit-worthy).
- Wedge depth / failure progress moved meaningfully (e.g. a wedge moving from progress=512 to progress=7680 is a commit-worthy partial fix even if success rate didn't improve).

**Reject rule** — any of:
- Metric regressed or unchanged within noise.
- Success rate dropped.
- Build failed and the fix wasn't easy to repair in-place.

On commit, write a commit message that:
1. Leads with the measurable effect (`mesh_bundle_loader: 10x RECEIVE_TIMEOUT — eliminate intermittent loader wedges`).
2. Cites before/after numbers verbatim.
3. Explains the *mechanism* the fix addresses — not just the diff.
4. Notes any safety reasoning (why it doesn't regress something else).
5. Links to prior related commits if this is part of a chain.

On reject, `git checkout HEAD -- <files>` and move on. Add a one-line note to the next iter's hypothesis list: "iter N tried X, no improvement / regressed — don't re-try without new evidence."

## When to stop

Stop the loop when any of:
- `iter_budget` is exhausted.
- Metric has hit a theoretical floor (UART line rate, fabric clock period, etc. — compute this up front so you know when you're there).
- Last 2 iters produced no meaningful gain AND hypotheses for the next iter are weak (< ~10% expected effect).
- A committed fix fully resolves the user's symptom (success rate 100%, metric at target).

On stop, produce a final summary that has: table of (iter → change → result → commit SHA), net effect vs. baseline, remaining headroom estimate (with reasoning), and the top 2 next-investigation directions if someone wants to push further.

## What NOT to do

- **Don't revert a recent PR wholesale to isolate a cause.** Those PRs are usually coordinated across files; reverting one typically re-exposes a prior bug. Bisect the files within a PR instead.
- **Don't commit a change that doesn't beat noise.** The cumulative cost of noise-committed patches is maintenance debt and false signal.
- **Don't rebuild the bitstream when you can test with the existing one.** Host-side or script-only changes are free iterations. Use them.
- **Don't chase a single run's result.** Always take `n_runs >= 3` and report distribution. One run doesn't tell you about flakiness.
- **Don't commit during a cooling-off period where the chip may be in a bad state** (e.g. right after a wedge). Reset the FPGA / re-flash first, then measure.
- **Don't skip the hypothesis step.** If you can't articulate a mechanism before patching, you're guessing. Pause and do RTL / source forensics first.

## Worked example — loader slowdown on `crockpot/loader-updates` (this is real history)

**Symptom:** `load-graph sample --inputs 16 ...` on fresh Nexys Video bitstream took 100 s avg (up from expected ~1-2 s) and wedged with `ERROR_IDLE_TIMEOUT` 67% of the time. Reproducer timing was visible in the JSON `"upload"` field of the CLI output.

**Baseline:** 6 runs, 2/6 succeeded @ 100-101 s, 4/6 wedged at progress 256-2816 in PHASE_TILE_COUNTS / PHASE_ROUTE_WORD.

**Iter 1 (rejected).** Hypothesised PR #38 pipeline registers on `tile_top.sv` `fanout_bank` write port caused per-write stalls. Reverted all 5 tile RTL files to pre-PR#38. Build passed (WNS +1.7 ns). Chip wedged at progress=256 — *worse than baseline*. Reverted. Learning: PR #38 tile changes are functionally required; reverting blind doesn't work.

**Iter 2 (partial keep, then rejected).** Kept `logical_neuron_state_bank.sv` at HEAD (commit c820954 fixes a real amplification bug), reverted the other 4. Sample upload matched baseline (~86 s); torch-pt wedge moved from progress 512 to 8192 (16× deeper). Decided against committing — sample improvement was zero.

**Iter 3 (rejected).** Partial restore (2 files HEAD, 2 reverted). Both tests wedged earlier — showed the PR #38 RTL is a coordinated set and can't be bisected file-by-file.

**Iter 4 (committed — 0a64da3).** Real RTL forensics: `tile_ingress.sv:699-710` reserved `host_slot_free_r` for every MSG_WRITE-with-target loader write to queue an ack response. The host never reads the ack (uses `DEBUG_WORD_LOADER_PROGRESS` polls instead), so every loader write round-tripped an unread response through the mesh, contending with incoming loader writes. Fix: skip slot-reserve for `CMD_ROUTE`/`CMD_WEIGHT`/`CMD_SYNAPSE` MSG_WRITEs. Result: 33% → 33% success, 101 s → 86 s avg, wedge depth 3-4× later. Committed.

**Iter 5 (committed — de397cf).** Increased `LOADER_RECEIVE_TIMEOUT_CYCLES` from 100 M to 1 G. Some per-burst stalls were over 2.5 s — increasing the chip's patience to 25 s kept slow uploads from tripping the idle timeout. Result: **33% → 100% success**, same 86 s avg. Wedges eliminated.

**Iter 6 (committed — c6ddbee).** Host-side: `TRANSPORT_TX_BURST_LIMIT` 32 → 256 in `session.rs`. Broker was yielding 8× per 256-packet loader burst. One-line change + cargo rebuild. Result: 86 s → 46 s avg (**47% faster**).

**Iter 7 (committed — bbcc4ba).** Host-side: `loader_upload_burst_words` factor 4× → 8×. Cut progress polls from 31 to 15 per upload. Result: 46 s → 26 s (43% faster).

**Iter 8 (committed — 47e8a5e).** 8× → 16×. 26 s → 19 s (27% faster).

**Iter 9 (committed — 45277cd).** 16× → 32×. 19 s → 16 s, best 10 s (16% faster — diminishing returns).

**Net:** 100 s 33% → 10-20 s 100%. **Upload −84%, wedge rate −100%.** Six committed fixes, three or four rejected attempts.

Key lessons the agent embeds:
- Forensics before patches (iter 4 was the real fix; iters 1-3 were wasted guesses from skipping it).
- Host-side rebuilds are free — do them greedily.
- A chain of small committed fixes beats one speculative big-bang revert.
- Measure distributions, not single points — the range (30-57 s vs 63-109 s) was as informative as the mean.

## Integration notes

- This agent is most useful when invoked via `/loop <instance of the pattern>` — the outer loop skill handles pacing; this agent owns the decision logic inside each iteration.
- If the user asks for a one-shot fix rather than iterative, escalate: "this looks like a 3-5 iter bisect; start the loop?" Don't burn a bitstream cycle on a single speculative fix without the measure-and-decide scaffold.
- Spawn sub-agents (e.g. `packet_trace`, code-exploration) for the hypothesis-forming step when the code path is unfamiliar. Don't spend the main context on directory-crawling.
