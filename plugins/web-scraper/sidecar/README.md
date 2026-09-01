# web-scraper sidecar (skeleton)

Minimal FastAPI sidecar implementing the plugin contract in ../SIDECAR_API_CONTRACT.md.

## What this skeleton already does

- Implements endpoints: /healthz, /v1/fetch, /v1/extract, /v1/crawl, /v1/session/close.
- Uses Scrapling fetchers for static/dynamic/stealthy modes.
- Enforces URL host safety checks (private/loopback rejected).
- Applies per-call allowed_domains checks.

## Gaps left intentionally for production hardening

- Persistent session pools keyed by session_id.
- Strict include_patterns/exclude_patterns handling for crawl.
- Robots policy controls.
- Accurate timings and richer error mapping.
- Browser dependency provisioning and warmup strategy for dynamic mode.

## Run locally

```bash
cd plugins/web-scraper/sidecar
python -m venv .venv
. .venv/bin/activate
pip install -r requirements.txt
uvicorn app:app --host 0.0.0.0 --port 8080
```
