# Real-task benchmark suites

`release-v1.json` is the small release suite. `periodic-v1.json` is the
larger recurring suite. Both cover isolated workspace coding tests,
source-cited research, memory recall, and automation proposals applied only to
an in-memory mock sink. Expected outputs and verifier definitions stay in the
suite, outside coding task workspaces.

Run the scorer offline to validate the installed harness and suite format:

```powershell
cargo run --offline -p execlaw-eval-harness -- benchmark `
  --suite evals/benchmark/release-v1.json `
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
the operator supplies the execution acknowledgement. Research and memory verifiers use explicit
required terms; research additionally requires a cited source ID. Automation
verifiers parse JSON effects and compare them to the expected sink records;
they make no external requests.
