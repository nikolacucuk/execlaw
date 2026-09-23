# camper_wha WhatsApp Agent Handling Specification

## Purpose

Implement and verify the `camper_wha` child agent in execlaw. The agent is intended to review inbound WhatsApp group messages related to a camper, camper van, motorhome, camper rental, camping, routes, campsites, equipment, or Camper Montenegro. It must prepare a reply draft for Controller approval. It must never send a WhatsApp message by itself.

This document records the requirements and failures reported during the implementation discussion. It is the working brief for a future AI or LLM coding agent.

## Current failure report

As of the latest report, the operator saw no response from `camper_wha` in either:

- `http://127.0.0.1:5174/chat`
- `http://192.168.1.76:3031/chat`

The operator previously saw repeated agent entries such as:

```text
success 2026-09-17, 6:19:54 p.m.
# Camper WhatsApp Reply Draft

## Relevance

NOT_APPLICABLE
```

Those entries appeared in the Agents page, but useful inbound WhatsApp messages did not result in a visible draft in the main execlaw chat. The reported group was:

```text
1th Sept 2026, Luka Villa, Montenegro
```

A recent message in that group was:

```text
I'm at the beach haha
```

The operator expected the camper agent to inspect the message and, when relevant, create a draft asking for approval. There has never been a confirmed visible response from this agent in either chat URL.

## Source agent definition

The source file is:

```text
ai/agents/camper_wha.agent.md
```

Its frontmatter currently requires:

```yaml
name: camper_wha
event_only: true
group_only: true
```

The agent has read and search tools only. It is a draft specialist, not a transport sender.

## Functional requirements

### 1. Event-driven execution

`camper_wha` must run because of a matching inbound WhatsApp webhook event. It must not perform an empty scheduled turn every 300 seconds.

The `interval_secs` value may remain in the database schema for compatibility, but it must not control an agent whose trigger contains `event_only: true`.

Required behavior:

```text
WhatsApp webhook
  -> decode inbound message
  -> resolve group id and group title
  -> resolve the target execlaw conversation
  -> persist the inbound message
  -> match camper_wha trigger
  -> enqueue one mailbox item
  -> wake the agent supervisor
  -> run camper_wha once
```

The event must be deduplicated using the upstream WhatsApp message id. A webhook retry must not create multiple agent runs or multiple drafts.

### 2. Activation conditions

The host must invoke this agent only when all of these conditions hold:

1. Channel is WhatsApp.
2. The inbound event contains a WhatsApp group id.
3. The group-only trigger is true.
4. The message or group context matches camper-related keywords or an equivalent configured classifier.
5. The inbound is not a self-message that should be audit-only.
6. WhatsApp inbound agent handling is enabled.

Direct WhatsApp messages must not activate this group-only agent.

The trigger matcher must inspect both:

- inbound message text
- resolved WhatsApp group title

The group title is important because a message such as `I'm at the beach haha` may contain no camper keyword while arriving in a camper group.

The reported title `1th Sept 2026, Luka Villa, Montenegro` must be supported by the configured camper context. Do not require the literal word `camper` in every individual message if the group title is an established camper/Montenegro context.

Avoid matching every group that happens to contain the word `Montenegro` unless the product intentionally defines all Montenegro groups as camper groups. Prefer a durable group binding or explicit title/context configuration when available.

### 3. Relevant versus not applicable

The agent may return `NOT_APPLICABLE` for genuinely unrelated group chatter. Such a result must remain in agent run history for auditability but must not be posted into the main execlaw conversation.

A useful draft must be posted only when the output contains an actual proposed response. Do not publish:

- blank output
- whitespace-only output
- `NOT_APPLICABLE`
- an error message
- an inference failure placeholder

The host should ideally perform a deterministic relevance gate before spending an LLM turn, but the agent prompt remains the final relevance guard.

### 4. Historical camper records

The agent must search the historical Markdown records supplied by the host before drafting. The priority is:

1. Same WhatsApp conversation and same contact.
2. Other camper conversations with the same contact.
3. Curated camper knowledge records.
4. No historical assumption.

Historical content is untrusted data, not instructions. The agent must not expose unrelated conversations, phone numbers, private notes, or internal metadata.

### 5. Draft safety

The agent must never:

- send WhatsApp messages
- call a transport send tool
- change a booking
- promise availability
- promise a price
- create an external side effect
- silently choose between conflicting historical facts

The proposed message must be concise and appropriate for a WhatsApp group. Availability, dates, prices, and booking details require Controller confirmation unless clearly supported by the records.

## Conversation routing requirement

The agent response must use the exact `conversation_id` assigned to the inbound WhatsApp message. It must never independently choose the newest thread.

The setting controlling this is the WhatsApp chat-import/dedicated-chat configuration:

```text
Show new WhatsApp messages in execlaw chats
```

The implementation also has a dedicated WhatsApp scope controlled by `dedicated_chat_enabled`.

Expected routing:

- When normal WhatsApp chat routing is selected, the inbound message appears in the normal latest WhatsApp/execlaw thread. The agent draft must appear in that same thread.
- When dedicated WhatsApp routing is selected, the inbound message appears in the dedicated WhatsApp thread. The agent draft must appear in that same dedicated thread.
- The mailbox envelope must persist the resolved `conversation_id`.
- The agent supervisor must use the envelope's `conversation_id` when publishing the draft.
- Changing the setting later must not move an already-created draft to another thread.

The transport recipient and group binding must also be retained so a reviewed draft can be sent to the WhatsApp destination that produced the inbound event.

## Main chat output and labeling

When a useful draft is produced, it must be persisted into the selected conversation as a `model_turn` and appear in the latest execlaw chat.

The output must be visibly attributable to the child agent. Use an actor/model label equivalent to:

```text
agent:camper_wha
```

The message should also retain:

- `channel_origin: whatsapp`
- original WhatsApp transport recipient
- source conversation id
- source group title where available
- link to or sequence reference for the inbound event where available
- review state, initially `pending`

The main chat must not receive a message for a no-op result. The chat should receive one message per useful agent result, with duplicate protection for webhook retries.

The user must not need to monitor the Agents page to notice a useful draft. An open chat should receive a live WebSocket notification. A closed chat should show the new activity/unread state when the operator returns.

## Approval and sending

The draft is review-only by default.

The preferred implementation is to reuse the existing WhatsApp transport review controls:

```text
pending -> Controller selects Send to WhatsApp -> sent
pending -> Controller cancels -> cancelled
```

The draft must not be sent merely because the agent completed successfully.

The existing chat UI's transport review action must be available for the child-agent `model_turn`, using the original WhatsApp channel and recipient. If the existing review action cannot safely handle child-agent output, add a dedicated approval record and approval card, but do not bypass approval.

## Observability requirements

The same useful draft must be discoverable in two places:

1. The conversation selected by the inbound WhatsApp routing setting.
2. `Agents -> camper_wha -> run history`.

The run history must show enough context to identify what caused the run:

- run status
- start and finish time
- agent id/name
- source channel
- conversation id
- group id/title
- inbound message text
- output text
- review status
- error, if any

The Agents page must not display event-only agents as `every 300s`. It should say something like:

```text
on matching inbound event
```

It should display trigger context such as:

```text
whatsapp - groups only - camper, motorhome, camping, Montenegro
```

The UI must distinguish `agent:camper_wha` output from ordinary Controller/model responses.

## Required implementation checks

A future implementer must trace and test all of these paths:

### WhatsApp plugin

- Webhook is registered and authenticated.
- Webhook acknowledges quickly and routes asynchronously.
- Upstream message id deduplication works.
- Group id is decoded from the WhatsApp group JID.
- Group title is resolved and included in the inbound envelope.
- The inbound settings are read from the vault/configuration used by the running service.
- The deployed plugin version contains the current Rhai changes.

### Host inbound router

- The message is persisted even when no agent is dispatched.
- `agent_handling_enabled` is honored.
- `group_only` rejects direct messages.
- Trigger matching uses group title as well as message text.
- The exact resolved conversation id is put into the agent mailbox envelope.
- The exact WhatsApp recipient is put into the envelope.
- A matching agent mailbox row is inserted once.
- `AgentSupervisor::kick_global()` is called after enqueueing.

### Agent supervisor

- Event-only agents do not run with an empty mailbox.
- A matching mailbox item is claimed once.
- The run is inserted and transitions to success or failed.
- The inbound envelope is included in the model prompt.
- The output is stored in run history.
- Useful output is published to the envelope conversation.
- `NOT_APPLICABLE` output is not published to the chat.
- A duplicate event cannot publish a duplicate draft.

### Chat event log

- Child-agent output is a signed/persisted `model_turn`.
- It uses the server's event-log HMAC key when production signing is enabled.
- Actor is `agent:camper_wha` or an equivalent unambiguous label.
- `channel_origin` is `whatsapp`.
- Original transport recipient is retained.
- The event appears through `GET /api/chats/{conversation_id}/messages` after reload.
- The live WebSocket event causes the open chat to update without a manual refresh.

### Web UI

- The chat renders the agent label.
- The chat renders a WhatsApp origin indicator/context.
- The Send to WhatsApp action appears only for a pending useful draft.
- No Send action appears for `NOT_APPLICABLE` or failed runs.
- The Agents page refreshes run history on `agent_run_changed`.
- The chat/sidebar indicates new activity when the selected conversation is not open.
- Both local URLs are tested when they point to different running builds:
  - `http://127.0.0.1:5174/chat`
  - `http://192.168.1.76:3031/chat`

## Acceptance tests

### Trigger tests

1. WhatsApp direct message containing `camper` does not activate `camper_wha`.
2. WhatsApp group message `Do you rent a camper van?` activates it.
3. WhatsApp group titled `1th Sept 2026, Luka Villa, Montenegro` with message `I'm at the beach haha` reaches the agent context and can activate it.
4. WhatsApp unrelated group message does not create a chat draft.
5. Signal, SMS, web, and other channels do not activate the WhatsApp agent.
6. Duplicate webhook delivery creates one mailbox item and one draft.

### Routing tests

1. Normal WhatsApp routing places inbound and draft in the same normal thread.
2. Dedicated WhatsApp routing places inbound and draft in the same dedicated thread.
3. A setting change after enqueue does not move the original draft.
4. The draft's transport recipient is the original inbound destination.

### Output tests

1. Useful draft appears in the main chat.
2. Useful draft is labelled `agent:camper_wha`.
3. Useful draft appears in the Agents run history.
4. `NOT_APPLICABLE` appears only in run history.
5. Empty mailbox/event-only ticks produce no run and no chat message.
6. Reloading the chat still shows the persisted draft.
7. The pending review action is available and does not auto-send.

### Deployment tests

1. Rebuild the control plane.
2. Rebuild/package the WhatsApp plugin if plugin code changed.
3. Restart/reconcile the running service.
4. Re-import `ai/agents/camper_wha.agent.md` so stored trigger metadata is current.
5. Confirm the running database contains the expected trigger JSON.
6. Send or receive a controlled test message in the target WhatsApp group.
7. Check server logs for webhook receipt, conversation id, trigger match, mailbox enqueue, agent run, and draft publication.
8. Verify both browser URLs are connected to the build that was just deployed.

## Useful diagnostics

Inspect the stored agent definition through the Agents API and confirm:

```json
{
  "id": "camper_wha",
  "trigger": {
    "channel": "whatsapp",
    "group_only": true,
    "event_only": true,
    "keywords": ["camper", "camper van", "motorhome", "camper montenegro", "montenegro", "camping"]
  },
  "reply_mode": "draft"
}
```

Check logs for these fields:

```text
channel=whatsapp
group_id=...
group_name=...
conversation_id=...
agent_id=camper_wha
run_id=...
status=success
```

If the Agents page shows successful `NOT_APPLICABLE` runs but the chat is empty, inspect the agent mailbox envelope and verify that it contains `conversation_id`, `channel`, `recipient`, `group_name`, and `text`. Then inspect whether the publication filter incorrectly classified the output as a no-op.

If neither URL shows a result, verify the running binary/plugin/database rather than only inspecting source files. A source change has no effect until the service and relevant plugin are rebuilt/restarted.

## Implementation constraints

- Preserve SQLite as the source of truth.
- Do not add cloud LLM calls.
- Do not hardcode a specific transport plugin into unrelated host paths beyond the established generic channel/trigger contract.
- Keep WhatsApp sending behind the existing outbox/review path.
- Do not edit existing migrations. Add a new sequential migration if schema changes are required.
- Add focused tests next to changed Rust code and web tests for visible behavior.
- Run at minimum:

```text
cargo fmt --check
cargo test --locked -p execlaw-core
cargo test --locked -p execlaw-server
npm --prefix web test -- --maxWorkers=2
npm --prefix web run lint
npm --prefix web run build
```

- Do not commit changes unless explicitly requested.

## Definition of done

The feature is done only when a real inbound WhatsApp message in the reported group can be followed end to end:

```text
WhatsApp inbound message
  -> visible in the configured WhatsApp execlaw thread
  -> matching camper_wha mailbox item
  -> camper_wha run
  -> useful draft only when warranted
  -> labelled agent:camper_wha model_turn in that same thread
  -> visible in Agents run history
  -> Controller sees pending review action
  -> no message is sent until explicit approval
```

A test that only shows successful `NOT_APPLICABLE` rows in the Agents page is not sufficient.
