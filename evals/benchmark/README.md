# Real-task benchmark suites

`release-v2.json` is the small release suite. `periodic-v2.json` is the
larger recurring suite. Both cover isolated workspace coding tests,
source-cited research, memory recall, and automation proposals applied only to
an in-memory mock sink. Research scoring requires every claim line to cite a
fetched source ID whose retained source text contains the claim's evidence
terms; a citation-shaped but unrelated source does not pass. The v1 suites are
retained as historical datasets with their original term-only verifier.
Expected outputs and verifier definitions stay in the suite, outside coding
task workspaces.

The H040 multi-file repair suite is
[`workspace-repair-v1.json`](workspace-repair-v1.json). It runs the workspace
read/search/patch, terminal, and LSP tools against a digest-pinned sandbox
image. Its offline fixture response is a deterministic tool-call trace for
validating the verifier; it is not model capability evidence. A live run asks
the local model to inspect the same fixture and produce a repair through those
tools.

Run the scorer offline to validate the installed harness and suite format:

```powershell
cargo run --offline -p execlaw-eval-harness -- benchmark `
  --suite evals/benchmark/release-v2.json `
  --output target/eval/release-fixture.json `
  --runs 3 --offline-fixture `
  --arm scorer-fixture --backend none --quantization none --hardware-tier cpu
```

Fixture mode checks the verifier pipeline; its results are not agent/model
performance evidence. For a real run, omit `--offline-fixture` and point
`--base-url` at the local OpenAI-compatible inference service. Record the
model, backend, quantization, hardware tier, run count, seed, and token budget.
If the suite includes coding tasks, also pass
`--allow-executing-generated-code`; those tasks compile and execute model
output in a temporary workspace. The subprocess drops environment variables
whose names indicate keys, tokens, secrets, passwords, credentials, or
endpoints. This temporary directory is not an OS security sandbox, so use a
trusted local model and reviewed suite.

The H040 workspace suite uses the approved container toolchain instead of the
legacy host subprocess verifier. To replay its deterministic multi-file repair
fixture through the terminal and diagnostics tools:

```powershell
cargo run --offline -p execlaw-eval-harness -- benchmark `
  --suite evals/benchmark/workspace-repair-v1.json `
  --output target/eval/workspace-repair-fixture.json `
  --workspace-image sha256:<local-image-id> `
  --approve-workspace-image --offline-fixture `
  --runs 1 --seed 1 --allow-executing-generated-code
```

`--approve-workspace-image` records Controller approval of that exact image
digest in the temporary benchmark database; it does not change the
installation-wide artifact policy. A live model run omits
`--offline-fixture` and uses the configured loopback inference endpoint. Its
result record includes final workspace hash, successful test-output hash and
exit evidence, LSP diagnostic count, tool-call names, and cumulative completion
tokens; it does not store source text or generated output.
The endpoint request uses temperature zero; the OpenAI-compatible request
schema currently has no seed field, so the recorded per-trial seed is also
included in the prompt and does not promise bit-for-bit model sampling.

For a paired baseline/candidate comparison, run both arms with the same suite,
seed, number of trials, model identity, and hardware identity. Pass the saved
baseline record as `--compare` on the candidate run. The harness rejects
mismatched dataset/model/backend/quantization/hardware identities and reports
per-arm Wilson intervals plus a paired success-rate delta interval. Failed
tasks and inference errors remain in the denominator. Generated answers are
not written to result files; failure records contain verifier summaries only.

Coding verifiers copy a suite fixture into a temporary directory, write the
model response to the declared artifact path, and execute only
`cargo test --offline --quiet`. This runs only for fixture responses or after
the operator supplies the execution acknowledgement. Research verifiers check
each cited claim against the cited fetched-source snapshot, then check required
answer terms. Memory verifiers use explicit required terms. Automation
verifiers parse JSON effects and compare them to the expected sink records;
they make no external requests.
