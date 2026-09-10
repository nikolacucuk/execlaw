# Plugin Development and Release Guide

This directory contains execlaw's in-tree plugins. A plugin is a ZIP bundle
with a `plugin.toml` manifest. Operators install or upgrade the ZIP through
**Settings -> Plugins**. The host discovers plugins through the manifest;
production host code must not special-case a plugin id.

For the complete manifest reference and runtime contracts, see
[`docs/plugins.md`](../docs/plugins.md).

## Layout

A plugin normally has this shape:

```text
plugins/<plugin-directory>/
  plugin.toml                 Manifest: id, version, tools, routes, services
  main.rhai                   Script-tier behavior, if applicable
  schemas/<tool>.json         JSON schemas for tool arguments
  ui/panel.tsx                Optional plugin settings UI source
  ui/panel.js                 Generated UI bundle included in the ZIP
```

`plugins/_shared/` is shared source, not an installable plugin. The package
script skips it.

## Create a plugin

1. Choose the closest existing plugin as a starting point:
   - `hello/` for a minimal subprocess plugin.
   - `google-places/` for an API-key integration.
   - `google-apps/` for an OAuth integration or identity provider.
   - `whatsapp/` for an HTTP-webhook transport with a supervised sidecar.
   - `signal/` or `sms-socket/` for a WebSocket-driven transport.
2. Create `plugins/<directory>/plugin.toml` with a unique `[plugin].id` and a
   version such as `0.1.0`.
3. Declare all public surfaces in the manifest: `[[tools]]`, optional schemas,
   `[[admin_routes]]`, `[[webhook_routes]]`, services, and UI panels.
4. Implement script-tier behavior in `main.rhai`, or the subprocess runtime
   described by the manifest.
5. Add focused tests beside the owning Rust code when the change introduces
   non-trivial behavior or an invariant.
6. Package the plugin and install its ZIP in the SPA.

Do not put secrets in `plugin.toml`. Operator configuration belongs in the
SQLite vault through the plugin settings API or `vault_get` / `vault_put`.

## Release a new plugin version

Every changed plugin must receive a new version before it is uploaded. The
plugin host uses the manifest id to identify an installed plugin, and the
version makes the resulting archive unambiguous.

1. Edit `plugins/<plugin>/plugin.toml`.
2. Increase `[plugin].version` using semantic versioning:
   - Patch, for fixes that preserve the plugin's public contract: `0.2.5` ->
     `0.2.6`.
   - Minor, for backwards-compatible tools or settings: `0.2.5` -> `0.3.0`.
   - Major, for incompatible manifest, tool, or data-contract changes:
     `0.2.5` -> `1.0.0`.
3. Update `description` when the shipped capability changes.
4. Update the plugin version/capability table in the repository `README.md` if
   it is affected.
5. Build and test the affected code before packaging.
6. Package the ZIP. Never hand-edit a generated archive.
7. Upgrade the installed plugin through **Settings -> Plugins**. The UI asks
   for confirmation before replacing an existing plugin id.
8. Re-enable a plugin after upgrade when it owns a sidecar, webhook, or
   long-running connection so its lifecycle hook reconciles runtime state.

Example for the WhatsApp history and inbound-event update:

```text
plugins/whatsapp/plugin.toml
version = "0.2.5"

-> edit implementation and increment to 0.2.6
-> package to dist/whatsapp-0.2.9.zip
-> upgrade that ZIP in Settings -> Plugins
```

## Package plugin ZIPs

Run the packaging command from the repository root. It builds all declared
plugin UI panels, packages every directory containing `plugin.toml`, and writes
an archive and SHA-256 checksum under `dist/`.

Windows PowerShell:

```powershell
npm ci --no-audit --no-fund
.\scripts\package-plugins.ps1
```

Linux, macOS, WSL, or Git Bash:

```bash
./scripts/package-plugins.sh
```

The expected output is:

```text
dist/<plugin-id>-<version>.zip
dist/<plugin-id>-<version>.zip.sha256
```

The script includes `main.rhai`, `plugin.toml`, schemas, and generated UI
assets. It intentionally excludes `ui/panel.tsx`, source maps, `node_modules`,
logs, build output, and other development files.

Verify an archive before distribution:

```bash
unzip -t dist/whatsapp-0.2.5.zip
shasum -a 256 -c dist/whatsapp-0.2.5.zip.sha256
```

On TrueNAS, create the archive in `/mnt/AI_Pool/execlaw-source`, upload it in
**Settings -> Plugins**, select the upgrade/reinstall action, then re-enable
sidecar-backed transports if required.

## Current plugins

| Plugin id | Tier | Primary capability |
| --- | --- | --- |
| `autoresearch` | script | Plans research experiments and analyzes/scorers results. |
| `discord` | script | Discord Bot Gateway transport with channel/DM messaging. |
| `finance-yahoo` | script | Yahoo Finance quotes, charts, symbol search, and market history. |
| `google-apps` | script | Google OAuth integration for Gmail, Calendar, Contacts, Tasks, and Drive; identity resolution. |
| `google-places` | script | Google Places text search, nearby search, and place details. |
| `plugin-hello` | subprocess | Minimal echo reference for the subprocess JSON-RPC plugin tier. |
| `humanizer-skills` | script | Installs reusable writing-style skills. |
| `identity-local-address-book` | subprocess | Local contact-list identity provider and trust resolution. |
| `obsidian-skills` | script | Installs Obsidian vault workflow skills. |
| `open-meteo` | script | Weather, marine, air-quality, climate, flood, geocoding, and elevation data. |
| `pushover` | script | One-way Pushover notifications to the operator. |
| `python-sandbox` | script | Persistent per-conversation Python execution through a supervised kernel gateway. |
| `signal` | script | Signal transport with QR/number pairing, messages, groups, and attachments. |
| `slack` | script | Multi-workspace Slack Socket Mode transport. |
| `sms-socket` | script | SMS/MMS transport through an Android LAN WebSocket gateway. |
| `tool-chain` | script | Deterministic multi-step tool plans with approval-gated execution. |
| `web-scraper` | script | JavaScript-capable webpage fetching, extraction, and link following via a sidecar. |
| `whatsapp` | script | WuzAPI-backed WhatsApp transport: event-driven inbound import, reviewable replies, direct-chat history, groups, attachments, and read receipts. |

## WhatsApp event behavior

WhatsApp uses WuzAPI webhooks for incoming messages. It does not poll WhatsApp
for new-message detection. With **Inbound message import** enabled in the
WhatsApp settings panel, each authenticated `Message` webhook is routed into
execlaw immediately. The inbound route persists the message in the appropriate
conversation and wakes matching always-on agents.

The `camper_wha` agent definition at
[`ai/agents/camper_wha.agent.md`](../ai/agents/camper_wha.agent.md) declares
`event_only: true` and `group_only: true`. Once imported through the Agents
screen, it runs only when a matching WhatsApp camper-related message is
received in a WhatsApp group; direct chats and unrelated group messages do not
activate it. It does not perform scheduled no-message runs. Its response is a
draft for Controller review.

The Agents page is event-driven as well. The controller wakes the agent
supervisor when a matching webhook or controller mailbox message is enqueued.
The supervisor publishes `agent_run_changed` WebSocket events for `running`,
`success`, and `failed` transitions. The page reloads the selected run history
when those events arrive; it does not poll the agent endpoint.

Disabling **Inbound message import** acknowledges WuzAPI webhook deliveries but
does not create conversations, display messages, or trigger agents.
