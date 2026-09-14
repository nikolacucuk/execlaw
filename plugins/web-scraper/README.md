# web-scraper plugin scaffold

This plugin adds advanced scraping tools backed by a supervised sidecar.

## Tools

- scraper.fetch_page
- scraper.extract
- scraper.clip_page
- scraper.follow_links
- scraper.session_close

`scraper.clip_page` returns bounded Obsidian-compatible Markdown with source
metadata. It deliberately returns content for review instead of writing to a
vault; a vault integration can decide where and when to persist it.

## Admin API

- GET /api/admin/plugins/web-scraper/config
- POST /api/admin/plugins/web-scraper/config
- POST /api/admin/plugins/web-scraper/test

## UI panel

Source: ui/panel.tsx
Build: node scripts/build-plugin-ui.mjs web-scraper

## Sidecar

See sidecar/ for a Python + Scrapling skeleton implementing the contract.
