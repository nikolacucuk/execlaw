# Web Scraper Sidecar API Contract (v0.1)

This document defines the minimal HTTP contract expected by the web-scraper plugin Rhai layer.

## Goals

- Keep execlaw trust and policy decisions in the host/plugin boundary.
- Keep scraping/browser dependencies inside the sidecar.
- Return stable, explicit JSON shapes for tool_result payloads.

## Common Rules

- Base URL: sidecar_url("scraper") from Rhai.
- Content type: application/json for all request/response bodies.
- Errors must be returned as non-2xx with:

```json
{
  "error": {
    "code": "string",
    "message": "string",
    "retryable": false
  }
}
```

- Sidecar must not call local/private hosts.
- Sidecar must enforce max response bytes and per-call timeout.
- Sidecar should include diagnostics fields when truncation/limits fire.

## Endpoints

### GET /healthz

Readiness and liveness probe.

Response 200:

```json
{
  "ok": true,
  "version": "0.1.0"
}
```

### POST /v1/fetch

Used by tool: scraper.fetch_page

Request:

```json
{
  "url": "https://example.com",
  "mode": "static",
  "session_id": "optional",
  "wait_for": "optional",
  "timeout_ms": 15000,
  "max_chars": 6000,
  "allowed_domains": ["example.com"]
}
```

Response 200:

```json
{
  "final_url": "https://example.com",
  "status": 200,
  "content_type": "text/html; charset=utf-8",
  "title": "Example Domain",
  "text": "...",
  "html_excerpt": "...",
  "truncated": false,
  "timings_ms": {
    "fetch": 120,
    "render": 0,
    "extract": 8
  }
}
```

### POST /v1/extract

Used by tool: scraper.extract

Request:

```json
{
  "url": "https://example.com/article",
  "mode": "dynamic",
  "session_id": "optional",
  "fields": [
    {
      "name": "headline",
      "selector": "h1",
      "selector_type": "css",
      "extract": "text",
      "all": false
    }
  ],
  "main_text": true,
  "include_links": true,
  "timeout_ms": 30000,
  "max_chars": 12000,
  "allowed_domains": ["example.com"]
}
```

Response 200:

```json
{
  "final_url": "https://example.com/article",
  "status": 200,
  "content_type": "text/html; charset=utf-8",
  "fields": {
    "headline": "..."
  },
  "main_text": "...",
  "links": [
    "https://example.com/a"
  ],
  "truncated": false,
  "timings_ms": {
    "fetch": 210,
    "render": 640,
    "extract": 32
  }
}
```

### POST /v1/crawl

Used by tool: scraper.follow_links

Request:

```json
{
  "seed_url": "https://example.com/docs",
  "mode": "static",
  "max_pages": 5,
  "max_depth": 1,
  "timeout_ms": 60000,
  "include_patterns": ["^https://example.com/docs"],
  "exclude_patterns": ["/login"],
  "extract": {
    "main_text": true,
    "fields": [
      { "name": "title", "selector": "h1" }
    ]
  },
  "allowed_domains": ["example.com"]
}
```

Response 200:

```json
{
  "seed_url": "https://example.com/docs",
  "visited": 5,
  "timed_out": false,
  "pages": [
    {
      "url": "https://example.com/docs/start",
      "status": 200,
      "title": "...",
      "main_text": "...",
      "fields": {
        "title": "..."
      }
    }
  ],
  "limits": {
    "max_pages": 5,
    "max_depth": 1,
    "timeout_ms": 60000
  }
}
```

### POST /v1/session/close

Used by tool: scraper.session_close

Request:

```json
{
  "session_id": "abc123"
}
```

Response 200:

```json
{
  "ok": true,
  "session_id": "abc123"
}
```

## Security Expectations

- Only http and https URLs are accepted.
- Reject loopback, private, link-local, multicast, and localhost domains.
- Follow redirects only when target URL also passes host checks.
- Optional robots policy can be added later; v0.1 leaves this operator-configurable.

## Versioning

- Backward-compatible additions: new optional fields only.
- Breaking changes: bump plugin version and include contract version in sidecar health response.
