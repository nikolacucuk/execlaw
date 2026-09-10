# WhatsApp Plugin Maintainer Guide

This document is the working guide for changes to the WhatsApp plugin.
Read it before modifying, packaging, or releasing `plugins/whatsapp/`. It
records the current architecture, deployment lessons, and the implemented
release behavior. It is not an operator guide and does not itself create a
new plugin version.

For generic plugin creation and packaging instructions, read
[`plugins/README.md`](../README.md). For manifest and runtime details, read
[`docs/plugins.md`](../../docs/plugins.md). For TrueNAS deployment, read
[`docs/truenas-docker.md`](../../docs/truenas-docker.md).

## Current version and layout

The current source manifest is `plugins/whatsapp/plugin.toml`. Its plugin id
is `whatsapp` and its current version is `0.2.13`.

```text
plugins/whatsapp/
  plugin.toml                         Manifest, tools, WuzAPI sidecar, routes
  main.rhai                           Pairing, webhook, inbound, tool behavior
  schemas/whatsapp.read_history.json  Direct/group history tool arguments
  schemas/whatsapp.send_message.json  Send/reply tool arguments
  ui/panel.tsx                        Settings panel source
  ui/panel.js                         Generated panel bundled into release ZIP
```

Do not edit `ui/panel.js` manually. Edit `ui/panel.tsx`, then rebuild it as
part of the packaging workflow.

## Architecture and non-negotiable behavior

WhatsApp is an event-driven transport. It must not periodically poll,
fetch, or pull WhatsApp messages to discover new inbound events.

```text
WhatsApp message
  -> WuzAPI receives it
  -> WuzAPI POSTs /api/webhooks/whatsapp/event?token=...
  -> plugin main.rhai::on_webhook_event authenticates and decodes it
  -> host_route_inbound_spawn returns a fast HTTP 200 response
  -> server generic_inbound::route_inbound persists/routes the message
  -> matching event-only agent mailbox entries are enqueued
  -> AgentSupervisor::kick_global wakes the agent supervisor
  -> matching agent runs from its mailbox and produces its configured result
```

The quick webhook acknowledgement is essential. WuzAPI retries webhook calls
that take too long. `host_route_inbound_spawn` must remain the webhook path;
do not replace it with synchronous `host_route_inbound`.

Every authenticated, enabled inbound `Message` must create a visible execlaw
chat event before group-address filtering or an agent decision. Group messages
that do not address the general agent are still silently committed so the
Controller can see them in the conversation. A blocked or cold contact is also
persisted through the host's appropriate inbound flow; policy controls what
runs afterward, not whether the operator can audit the message.

Messages sent by the linked account (`IsFromMe: true`) are intentionally
ignored. Importing them would cause the agent to react to the operator's own
outbound WhatsApp messages.

## WuzAPI and TrueNAS requirements

The WuzAPI sidecar is dynamically created by execlaw through the mounted Docker
socket. On Linux and TrueNAS it must receive this Docker host mapping:

```text
host.docker.internal:host-gateway
```

The control-plane Compose `extra_hosts` setting does not automatically apply to
dynamically created sidecars. The container-manager creation path is responsible
for the sidecar mapping. Verify a deployed TrueNAS installation with:

```bash
sudo docker inspect execlaw-sidecar-whatsapp-wuzapi \
  --format '{{json .HostConfig.ExtraHosts}}'
```

A `null` result means WuzAPI cannot resolve the control-plane callback hostname;
new inbound WhatsApp messages will not be delivered to execlaw. The expected
result includes `host.docker.internal:host-gateway`.

The plugin registers WuzAPI's callback at:

```text
http://host.docker.internal:3031/api/webhooks/whatsapp/event?token=<secret>
```

The secret lives in execlaw's vault as `webhook_secret`. Do not log it, put it
in documentation examples, or replace the token check with an open webhook.

The WuzAPI user token is also vault-backed. Pairing state persists in the
supervised sidecar volume at `state://data` mounted as `/app/dbdata`.

## Current features

The current plugin provides:

- QR device pairing and linked-account status.
- Event-driven inbound direct-message and group-message delivery by WuzAPI
  webhook.
- Default-enabled **Inbound message import** setting. When disabled, the
  webhook is acknowledged but no conversation event or agent work is created.
- Review-first inbound reply mode. `inbound_reply_mode=review` prevents
  automatic external replies; `automatic` permits the host reply bridge to
  send.
- `whatsapp.send_message` and `whatsapp.reply` for direct messages and groups.
- `whatsapp.list_groups`, `whatsapp.create_group`,
  `whatsapp.add_group_members`, and `whatsapp.leave_group`.
- Inbound read receipts and attachments.
- `whatsapp.read_history` for WuzAPI-retained direct or group history.
- One shared current execlaw operator conversation for all WhatsApp direct and
  group inbound messages, selected by latest activity.
- Separate transport bindings for each direct contact and group so a reviewed
  reply still knows its WhatsApp destination.
- Per-reply review controls for every pending WhatsApp model response, with
  each approval routed to the recipient that produced that response.
- Independent settings for importing messages into execlaw chats and for
  allowing agent/LLM handling of imported messages.
- Local SPA unread indicators for imported inbound direct and group messages;
  opening the conversation clears the indicator.

`whatsapp.read_history` is the source for a current WhatsApp "latest message"
request. execlaw's `read_conversation_history` reads only execlaw's event log
and may be older than the actual WhatsApp conversation.

For live history, the required tool sequence is:

1. For a named group, call `whatsapp.list_groups` and obtain the exact group
   JID ending in `@g.us`.
2. Call `whatsapp.read_history` with `refresh: true`.
3. Wait at least the returned `retry_after_seconds`.
4. Call `whatsapp.read_history` with `refresh: false`.
5. Treat only the first record from the second call as the latest message.

WuzAPI acknowledges a history-sync request before WhatsApp necessarily sends
newer records. Do not label old cached data as the latest message.

## Event-only camper agent

[`ai/agents/camper_wha.agent.md`](../../ai/agents/camper_wha.agent.md) is a
WhatsApp camper-message draft specialist. Its frontmatter sets
`event_only: true`. Importing that Markdown through the Agents UI/API produces
an agent whose WhatsApp trigger matches camper-related keywords and whose reply
mode is `draft`.

An event-only agent must not run an interval-based "no new mailbox messages"
turn. It becomes due only when `generic_inbound::enqueue_triggered_agents`
receives a matching inbound webhook event. The supervisor wake signal is an
internal scheduling notification, not a WhatsApp polling mechanism.

## Release 0.2.13: Per-reply WhatsApp approval

Every WhatsApp-originated model response in the shared execlaw chat now has
its own **Send to WhatsApp** and **Cancel reply** controls. Approving one
response does not approve, cancel, or redirect the others. Each inbound event
stores its originating phone number or group JID, and the reviewed send uses
that stored recipient rather than the latest binding for the entire shared
conversation. This supports multiple direct chats and groups arriving at the
same time.

The recipient metadata is new event data. Older events created before this
release may fall back to the conversation's latest binding when their original
recipient was not persisted.

## Release 0.2.12: Separate chat import and agent handling

The WhatsApp settings panel now exposes two independent controls:

- **Show new WhatsApp messages in execlaw chats** controls whether inbound
  messages are imported at all.
- **Enable agent handling of new WhatsApp messages** controls matching-agent
  triggers and general LLM turns. When disabled, messages remain visible in
  execlaw but no agent or LLM response is generated.

The existing `inbound_import_enabled` setting remains the chat-import key for
backward compatibility. The new `inbound_agent_handling_enabled` setting
defaults to enabled when absent.

## Release 0.2.11: One shared WhatsApp operator thread

This release makes the transport-wide WhatsApp scope explicit in the host
resolver path and regression-tests convergence across long gaps. Direct
contacts and groups retain separate transport bindings for reviewed replies,
but their imported messages use the newest shared execlaw operator chat.

## Release 0.2.10: WhatsApp routing and review controls

This release routes new WhatsApp activity into the newest eligible active
execlaw conversation and keeps review controls attached only to the latest
WhatsApp-originated proposed reply. The transcript exposes sender, phone,
and group context above inbound messages. The existing unread-preservation
behavior remains unchanged.

## Release 0.2.9: WhatsApp unread preservation and message styling

Inbound messages are no longer acknowledged with WuzAPI's `markread` call.
WhatsApp therefore retains its unread state until the operator reads the
message in WhatsApp. execlaw also keeps the shared WhatsApp conversation's
local unread badge active when new inbound activity arrives, including while
the conversation is open.

WhatsApp-originated messages in the execlaw transcript use a light green
background, making them distinct from messages typed directly in execlaw.

## Release 0.2.8: shared operator thread and reviewable replies

All imported WhatsApp messages now use one current execlaw operator thread,
regardless of whether they came from a direct chat or group. The latest
WhatsApp binding remains attached to that thread so an explicitly reviewed
reply is sent to the chat that produced the latest inbound message.

The normal execlaw composer remains available in the thread. In review mode,
the latest agent response also has a **Send to WhatsApp** action; it sends the
visible response verbatim and does not start another agent turn.

## Release 0.2.7: unread visibility and shared-thread foundations

This release addresses the two user-facing requirements below. The unread state
remains local to the SPA, following the existing `has_unread` convention; it is
not a WhatsApp read receipt and does not send anything back to WhatsApp.

### 1. Mark imported WhatsApp conversations unread

When an enabled inbound WhatsApp message arrives and execlaw persists it, the
shared WhatsApp operator conversation becomes unread in the SPA.
The Controller must be able to see that a message arrived and open the relevant
chat to read it.

Requirements:

- Mark the conversation unread for an inbound WhatsApp message, including a
  group message that the general assistant does not dispatch.
- Do not mark a conversation unread for ignored `IsFromMe: true` echo events.
- Do not mark anything unread when **Inbound message import** is disabled.
- Preserve an unread indication until the Controller opens/acknowledges that
  conversation according to the SPA's existing read-state convention.
- Ensure new inbound activity changes sidebar ordering or visibility so the
  conversation is discoverable without manually searching old chats.
- Keep this state local to the execlaw operator UI; it must not alter WhatsApp
  read receipts or send an outbound message.

Investigate the existing conversation/sidebar read-state model before adding a
new table or ad hoc browser-only state. The inbound owning path is
`crates/server/src/generic_inbound.rs::route_inbound`; conversation persistence
is under `crates/server/src/chats.rs`; the SPA chat/sidebar behavior is under
`web/src/`. Add backend and frontend tests for the full inbound-to-unread path.

### 2. Reuse one active execlaw conversation for WhatsApp

New WhatsApp messages from any direct contact or group continue in the shared
current execlaw operator conversation. They must not mint a new visible chat
merely because the sender, group, or idle time changed.

The generic `ConversationResolver::resolve_or_mint` in
`crates/core/src/transport_conversations.rs` still supports idle rotation for
ordinary transports. WhatsApp avoids that rotation by supplying the shared
transport scope with no idle timeout, then retargets that scope to the newest
eligible active operator chat.

Required behavior for WhatsApp:

- Resolve the shared operator conversation through the transport-wide
  `conversation_scope = "whatsapp"` key and continue it regardless of idle
  time, unless the Controller explicitly starts/archives/rotates a
  conversation through a future deliberate user action.
- Preserve the stable contact or `@g.us` group identity in the transport
  binding used for reply delivery; the binding is not the execlaw chat key.
- Update `last_message_at` and the conversation's activity timestamp so the
  continued conversation is the newest visible chat.
- Preserve existing idle-window rotation for other transports unless their own
  contract explicitly changes.
- Do not merge different contacts or different groups at the transport-binding
  layer. Their inbound transcript events may share the operator conversation,
  but reviewed replies remain scoped to the latest inbound destination.
- Keep the Controller's fixed controller thread behavior intact.

The shared behavior is implemented through the transport-level scope and an
explicit resolver input rather than a per-contact special case. Prefer this
kind of transport-level configuration or resolver input such as
"no idle rotation" over a hardcoded `if channel == "whatsapp"` branch in
shared host code. The host must remain plugin-generic. A manifest-declared
transport setting, propagated into generic inbound routing, is the preferred
architecture if the manifest schema supports an additive field.

Add regression tests that prove:

1. Two WhatsApp messages from different direct contacts use the shared
  conversation scope.
2. Messages from direct contacts and groups use the same shared operator
  conversation after latest-active-chat retargeting.
3. The stable contact and group bindings remain distinct for reviewed reply
  delivery.
4. The continued conversation becomes unread and is surfaced as current in the
   operator chat UI after each inbound message.
5. The inbound import toggle disables both the conversation event and agent
   trigger without causing WuzAPI retries.

## Release procedure

Use this process for every WhatsApp plugin release, including the requested
unread/stable-conversation work.

1. Read this file, [`plugins/README.md`](../README.md), and the current
   `plugin.toml` before editing.
2. Trace the controlling code path. If the behavior is determined by the host
   routing, event log, or SPA, edit the owning host/UI module as well as the
   plugin; do not place a fragile workaround only in Rhai.
3. Preserve the webhook authentication, fast acknowledgement,
   `host_route_inbound_spawn`, inbound idempotency, and `IsFromMe` protection.
4. Add narrow tests covering the changed invariant. For a UI-visible behavior,
   include backend persistence/routing coverage and frontend/sidebar coverage.
5. Increment `plugins/whatsapp/plugin.toml` from its current version. Use a
   patch version for a compatible fix and update the plugin description and
   root `README.md` table if capability text changed.
6. Rebuild `ui/panel.js` from `ui/panel.tsx`:

   ```powershell
   & 'C:\Program Files\nodejs\node.exe' .\scripts\build-plugin-ui.mjs whatsapp
   ```

7. Run the focused Rust and web tests when their toolchains are available.
   At minimum, run the WhatsApp Rhai test and the affected server/core tests:

   ```bash
   cargo test -p execlaw-script --test whatsapp_plugin
   cargo test -p execlaw-core
   cargo test -p execlaw-server
   cd web && npm test && npm run lint
   ```

8. Package from the repository root:

   ```bash
   ./scripts/package-plugins.sh
   ```

   On Windows, use the PowerShell packaging script if available. The packaging
   script builds every declared UI panel and creates
   `dist/whatsapp-<version>.zip` plus its `.sha256` file.

9. Verify the intended package, not a previous ZIP:

   ```bash
   unzip -t dist/whatsapp-<version>.zip
   unzip -p dist/whatsapp-<version>.zip plugin.toml | grep '^version'
   shasum -a 256 -c dist/whatsapp-<version>.zip.sha256
   ```

10. On TrueNAS, deploy any host Rust changes by rebuilding/recreating the
    control-plane image. Upload the new ZIP in **Settings -> Plugins**, choose
    upgrade/reinstall, and re-enable WhatsApp so it re-registers its callback
    and reconciles the sidecar.
11. Validate with a real incoming WhatsApp message from a test contact and a
    group message. Check that it appears in the existing conversation, has an
    unread indicator, wakes a matching event-only agent, and does not generate
    a duplicate reply or a new chat after an idle period.

Do not commit or push without explicit user approval.
