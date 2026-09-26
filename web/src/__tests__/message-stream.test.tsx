// Tests for the chat MessageStream:
// - empty / loading states render
// - long messages show a Read more toggle
// - channel-origin icon renders per message (default "web")
// - streaming bubble shows the typing cursor

import { afterEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AuthContext } from "../auth/AuthContext";
import {
    MessageStream,
    formatMessageTimestamp,
    stripSkillPrependBlock,
} from "../chat/MessageStream";
import {
    __resetChatStore,
    appendMessage,
    appendStreamingToken,
    setMessages,
} from "../chat/store";

afterEach(() => {
    __resetChatStore();
    localStorage.removeItem("execlaw.chat.appearance");
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
});

const baseMsg = (
    seq: number,
    text: string,
    kind = "user_msg",
): {
    seq: number;
    kind: string;
    text: string | null;
    actor: string | null;
    committed_at: number;
} => ({
    seq,
    kind,
    text,
    actor: "controller",
    committed_at: 0,
});

describe("MessageStream", () => {
    it("keeps the classic stream as the default", () => {
        setMessages("classic", [baseMsg(1, "hello")]);
        render(<MessageStream conversationId="classic" />);
        expect(document.querySelector(".execlaw-nexus")).toBeNull();
        expect(screen.queryByRole("navigation", { name: "Message source navigation" })).toBeNull();
    });

    it("navigates recorded reply links and source matches without hiding messages", () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        const scrollIntoView = vi.fn();
        Element.prototype.scrollIntoView = scrollIntoView;
        setMessages("nexus", [
            { ...baseMsg(1, "Signal question"), channel_origin: "signal" },
            { ...baseMsg(2, "An unrelated incoming message"), channel_origin: "email" },
            { ...baseMsg(3, "Answer", "model_turn"), reply_to_seq: 1 },
        ]);
        render(<MessageStream conversationId="nexus" />);
        fireEvent.click(screen.getByRole("button", { name: /Reply to #1/ }));
        expect(document.activeElement).toHaveAttribute("data-message-seq", "1");
        expect(scrollIntoView).toHaveBeenCalled();
        fireEvent.change(screen.getByRole("combobox", { name: "Message source" }), { target: { value: "Email" } });
        fireEvent.click(screen.getByRole("button", { name: "Next matching message" }));
        expect(document.activeElement).toHaveAttribute("data-message-seq", "2");
        expect(screen.getByText("Signal question", { selector: "p" })).toBeInTheDocument();
        expect(screen.getByText("Answer")).toBeInTheDocument();
        fireEvent.click(screen.getByRole("button", { name: "Previous matching message" }));
        expect(document.activeElement).toHaveAttribute("data-message-seq", "2");
    });

    it("keeps the latest grouped reply visible and expands hidden parents on jump", () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        Element.prototype.scrollIntoView = vi.fn();
        setMessages("group", [
            { ...baseMsg(1, "First Signal message"), channel_origin: "signal", transport_group: "Planning" },
            { ...baseMsg(2, "Second WhatsApp message"), channel_origin: "whatsapp", transport_group: "Planning" },
            { ...baseMsg(3, "Latest reply", "model_turn"), transport_group: "Planning", reply_to_seq: 2 },
            { ...baseMsg(4, "Outside group"), channel_origin: "discord" },
        ]);
        render(<MessageStream conversationId="group" />);
        const toggle = screen.getByRole("button", { name: /Planning.*3 messages.*3 sources/ });
        fireEvent.click(toggle);
        expect(toggle).toHaveAttribute("aria-expanded", "false");
        expect(screen.queryByText("First Signal message")).toBeNull();
        expect(screen.queryByText("Second WhatsApp message", { selector: "p" })).toBeNull();
        expect(screen.getByText("Latest reply")).toBeInTheDocument();
        expect(screen.getByText("Outside group")).toBeInTheDocument();
        fireEvent.click(screen.getByRole("button", { name: /Reply to #2/ }));
        expect(toggle).toHaveAttribute("aria-expanded", "true");
        expect(screen.getByText("Second WhatsApp message", { selector: "p" })).toBeInTheDocument();
        expect(document.activeElement).toHaveAttribute("data-message-seq", "2");
    });

    it("collapses repeated group labels independently", () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        setMessages("repeated-group", [
            { ...baseMsg(1, "Earlier group start"), transport_group: "Planning" },
            { ...baseMsg(2, "Earlier group end"), transport_group: "Planning" },
            baseMsg(3, "Intervening message"),
            { ...baseMsg(4, "Later group start"), transport_group: "Planning" },
            { ...baseMsg(5, "Later group end"), transport_group: "Planning" },
        ]);
        render(<MessageStream conversationId="repeated-group" />);
        const toggles = screen.getAllByRole("button", { name: /Planning.*2 messages/ });
        fireEvent.click(toggles[0]);
        expect(screen.queryByText("Earlier group start")).toBeNull();
        expect(screen.getByText("Earlier group end")).toBeInTheDocument();
        expect(screen.getByText("Later group start")).toBeInTheDocument();
        expect(toggles[1]).toHaveAttribute("aria-expanded", "true");
    });

    it("searches without reordering and fades only messages without recorded relationships", () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        Element.prototype.scrollIntoView = vi.fn();
        setMessages("relationships", [
            { ...baseMsg(1, "Signal question"), channel_origin: "signal" },
            { ...baseMsg(2, "Discord aside"), channel_origin: "discord" },
            { ...baseMsg(3, "Agent answer", "model_turn"), reply_to_seq: 1 },
        ]);
        render(<MessageStream conversationId="relationships" />);
        const sourceOptions = () => [...screen.getByRole("combobox", { name: "Message source" }).querySelectorAll("option")].map((option) => option.textContent);
        expect(sourceOptions()).toEqual(["All sources", "Signal", "Discord", "execlaw"]);
        fireEvent.change(screen.getByRole("combobox", { name: "Source order" }), { target: { value: "name" } });
        expect(sourceOptions()).toEqual(["All sources", "Discord", "execlaw", "Signal"]);
        fireEvent.click(screen.getByRole("button", { name: "Relationships" }));
        expect(screen.getByText("Discord aside").closest("[data-message-seq]")).toHaveClass("is-unrelated");
        expect(screen.getByText("Signal question", { selector: "p" }).closest("[data-message-seq]")).not.toHaveClass("is-unrelated");
        fireEvent.change(screen.getByRole("searchbox", { name: "Search messages" }), { target: { value: "answer" } });
        expect(screen.getByText("0 / 1")).toBeInTheDocument();
        fireEvent.click(screen.getByRole("button", { name: "Next matching message" }));
        expect(document.activeElement).toHaveAttribute("data-message-seq", "3");
        expect([...document.querySelectorAll("[data-message-seq]")].map((node) => node.getAttribute("data-message-seq"))).toEqual(["1", "2", "3"]);
    });

    it("loads older search hits and saves explicit scoped relationships", async () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        Element.prototype.scrollIntoView = vi.fn();
        setMessages("remote", [baseMsg(3, "Recent response", "model_turn")]);
        const fetchMock = vi.fn(async (input: RequestInfo | URL, options?: RequestInit) => {
            const path = String(input);
            if (path.endsWith("/nexus") && options?.method === "GET")
                return Response.json({ annotations: [], views: [] });
            if (path.includes("/messages/search?"))
                return Response.json({ matches: [{ seq: 1, text: "Older shipment", source: "web", committed_at: 1 }], has_more: false });
            if (path.includes("around=1"))
                return Response.json({ conversation_id: "remote", messages: [baseMsg(1, "Older shipment"), baseMsg(3, "Recent response", "model_turn")] });
            if (path.endsWith("/nexus/annotation"))
                return Response.json({ saved: true });
            return Response.json({ error: "unknown request" }, { status: 404 });
        });
        vi.stubGlobal("fetch", fetchMock);
        render(<AuthContext.Provider value={{ status: "authenticated", getAccessToken: () => "test-token" } as never}>
            <MessageStream conversationId="remote" />
        </AuthContext.Provider>);
        await screen.findByRole("button", { name: "Search full history" });
        fireEvent.change(screen.getByRole("searchbox", { name: "Search messages" }), { target: { value: "shipment" } });
        fireEvent.click(screen.getByRole("button", { name: "Search full history" }));
        fireEvent.click(await screen.findByRole("button", { name: /web #1.*Older shipment/ }));
        await waitFor(() => expect(screen.getByText("Older shipment", { selector: "p" })).toBeInTheDocument());
        await waitFor(() => expect(document.activeElement).toHaveAttribute("data-message-seq", "1"));
        fireEvent.click(screen.getByText("Organize message #1"));
        const editor = screen.getByText("Organize message #1").closest("details")!;
        fireEvent.change(editor.querySelector('input[placeholder="Branch name"]')!, { target: { value: "shipment" } });
        fireEvent.change(editor.querySelector('input[placeholder="Comma-separated tags"]')!, { target: { value: "urgent" } });
        fireEvent.change(editor.querySelector("select")!, { target: { value: "3" } });
        fireEvent.click(screen.getAllByRole("button", { name: "Add relationship" })[0]);
        fireEvent.click(screen.getAllByRole("button", { name: "Save organization" })[0]);
        await waitFor(() => expect(fetchMock).toHaveBeenCalledWith(expect.stringContaining("/nexus/annotation"), expect.objectContaining({
            body: expect.stringContaining('"branch_id":"shipment"'),
        })));
        expect(screen.getByText("Branch: shipment")).toBeInTheDocument();
    });

    it("collapses a durable branch across interleaved sources and expands a reply target", async () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        Element.prototype.scrollIntoView = vi.fn();
        setMessages("interleaved", [
            { ...baseMsg(1, "Signal origin"), channel_origin: "signal" },
            { ...baseMsg(2, "Discord interlude"), channel_origin: "discord" },
            { ...baseMsg(3, "Agent latest", "model_turn"), reply_to_seq: 1 },
        ]);
        vi.stubGlobal("fetch", vi.fn(async () => Response.json({ annotations: [
            { seq: 1, branch_id: "shipment", tags: [], links: [] },
            { seq: 3, branch_id: "shipment", tags: [], links: [] },
        ], views: [] })));
        render(<AuthContext.Provider value={{ status: "authenticated", getAccessToken: () => "test-token" } as never}>
            <MessageStream conversationId="interleaved" />
        </AuthContext.Provider>);
        fireEvent.click(await screen.findByRole("button", { name: "Collapse branch shipment" }));
        expect(screen.queryByText("Signal origin", { selector: "p" })).toBeNull();
        expect(screen.getByText("Discord interlude", { selector: "p" })).toBeInTheDocument();
        expect(screen.getByText("Agent latest", { selector: "p" })).toBeInTheDocument();
        fireEvent.click(screen.getByRole("button", { name: /Reply to #1/ }));
        expect(await screen.findByText("Signal origin", { selector: "p" })).toBeInTheDocument();
        expect(document.activeElement).toHaveAttribute("data-message-seq", "1");
    });

    it("buffers arrivals by source when reading earlier messages", async () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        setMessages("buffered", [
            { ...baseMsg(1, "Earlier Signal"), channel_origin: "signal", transport_group: "Planning" },
            { ...baseMsg(2, "Earlier agent", "model_turn"), transport_group: "Planning" },
        ]);
        render(<MessageStream conversationId="buffered" />);
        const stream = screen.getByTestId("message-stream");
        Object.defineProperties(stream, {
            scrollHeight: { configurable: true, value: 500 },
            clientHeight: { configurable: true, value: 100 },
            scrollTop: { configurable: true, writable: true, value: 0 },
        });
        fireEvent.scroll(stream);
        appendMessage("buffered", { ...baseMsg(3, "New Signal"), channel_origin: "signal", transport_group: "Planning" });
        await waitFor(() => expect(screen.getByRole("button", { name: /1 new from Signal/ })).toBeInTheDocument());
        expect(screen.getByRole("button", { name: /Planning.*3 messages.*1 new/ })).toBeInTheDocument();
        expect(stream.scrollTop).toBe(0);
    });

    it("discards full-history results from an obsolete query", async () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        setMessages("search-race", [baseMsg(1, "Recent")]);
        let completeSearch!: (response: Response) => void;
        const pendingSearch = new Promise<Response>((resolve) => { completeSearch = resolve; });
        vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL) => String(input).includes("/messages/search?")
            ? pendingSearch : Promise.resolve(Response.json({ annotations: [], views: [] }))));
        render(<AuthContext.Provider value={{ status: "authenticated", getAccessToken: () => "test-token" } as never}>
            <MessageStream conversationId="search-race" />
        </AuthContext.Provider>);
        fireEvent.change(screen.getByRole("searchbox", { name: "Search messages" }), { target: { value: "older" } });
        fireEvent.click(screen.getByRole("button", { name: "Search full history" }));
        fireEvent.change(screen.getByRole("searchbox", { name: "Search messages" }), { target: { value: "newer" } });
        await act(async () => completeSearch(Response.json({ matches: [{ seq: 5, text: "Stale result", source: "web", committed_at: 1 }], has_more: false })));
        expect(screen.queryByRole("region", { name: "Full history search results" })).toBeNull();
    });

    it("reports an empty full-history search", async () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        setMessages("no-matches", [baseMsg(1, "Recent")]);
        vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => String(input).includes("/messages/search?")
            ? Response.json({ matches: [], has_more: false })
            : Response.json({ annotations: [], views: [] })));
        render(<AuthContext.Provider value={{ status: "authenticated", getAccessToken: () => "test-token" } as never}>
            <MessageStream conversationId="no-matches" />
        </AuthContext.Provider>);
        fireEvent.change(screen.getByRole("searchbox", { name: "Search messages" }), { target: { value: "missing" } });
        fireEvent.click(screen.getByRole("button", { name: "Search full history" }));
        expect(await screen.findByText("No matches in this conversation")).toBeInTheDocument();
    });

    it("does not infer replies and disables links to unloaded sources", () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        setMessages("missing", [
            baseMsg(2, "Question"),
            baseMsg(3, "Unlinked response", "model_turn"),
            { ...baseMsg(4, "Older source response", "model_turn"), reply_to_seq: 1 },
        ]);
        render(<MessageStream conversationId="missing" />);
        expect(screen.getByRole("button", { name: /Reply to #1.*Source not loaded/ })).toBeDisabled();
        expect(screen.queryByRole("button", { name: /Reply to #2/ })).toBeNull();
    });

    it.each(["slack", "discord", "custom-database", "constructor"])("preserves %s channel identity", (channel) => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        setMessages("extensible", [{ ...baseMsg(1, "Source payload"), channel_origin: channel }]);
        render(<MessageStream conversationId="extensible" />);
        expect(screen.getByTestId("channel-origin")).toHaveAttribute("data-channel", channel);
        expect(screen.queryByRole("option", { name: "Web" })).toBeNull();
    });

    it("restores Classic immediately on a preference change in another tab", () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        setMessages("restore", [baseMsg(1, "Preserved message")]);
        render(<MessageStream conversationId="restore" />);
        expect(document.querySelector(".execlaw-nexus")).toBeInTheDocument();
        localStorage.setItem("execlaw.chat.appearance", "classic");
        fireEvent(window, new StorageEvent("storage", { key: "execlaw.chat.appearance", newValue: "classic" }));
        expect(document.querySelector(".execlaw-nexus")).toBeNull();
        expect(screen.getByText("Preserved message")).toBeInTheDocument();
    });

    it("does not expose internal skill preambles in reply previews", () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        setMessages("skill-reply", [
            { ...baseMsg(1, '<skill name="writing">Internal instructions</skill>\n\nActual question'), applied_skill_names: ["writing"] },
            { ...baseMsg(2, "Answer", "model_turn"), reply_to_seq: 1 },
        ]);
        render(<MessageStream conversationId="skill-reply" />);
        const link = screen.getByRole("button", { name: /Reply to #1/ });
        expect(link).toHaveAttribute("title", "#1: Actual question");
        expect(link).not.toHaveTextContent("Internal instructions");
    });

    it("honors hidden tools in Nexus and keeps streaming visible", () => {
        localStorage.setItem("execlaw.chat.appearance", "nexus");
        setMessages("tools", [baseMsg(1, "Question"), { ...baseMsg(2, "secret tool payload", "tool_result"), actor: "MCP" }]);
        appendStreamingToken("tools", "Live reply");
        render(<MessageStream conversationId="tools" showToolResults={false} />);
        expect(screen.queryByText("secret tool payload")).toBeNull();
        expect(screen.queryByRole("option", { name: "MCP" })).toBeNull();
        expect(screen.getByTestId("streaming-bubble")).toHaveTextContent("execlaw");
        expect(screen.getByText("Live reply")).toBeInTheDocument();
    });

    it("shows the loading state when messages are unset", () => {
        // No setMessages call → messages[conv] is null.
        render(<MessageStream conversationId="conv-x" />);
        expect(screen.getByText(/Loading messages/i)).toBeInTheDocument();
    });

    it("shows the empty hint when messages array is empty", () => {
        setMessages("conv-empty", []);
        render(<MessageStream conversationId="conv-empty" />);
        expect(
            screen.getByText(/No messages yet\. Type below/i),
        ).toBeInTheDocument();
    });

    /// 2026-04-28 — `web` (default origin) deliberately renders NO
    /// channel icon. The icon was visual noise for the common case;
    /// the test now pins the cleaner contract: web origin = no icon.
    it("does NOT render a channel-origin icon for default web messages", () => {
        setMessages("conv-1", [
            baseMsg(1, "hello"),
            baseMsg(2, "world", "model_turn"),
        ]);
        render(<MessageStream conversationId="conv-1" />);
        expect(screen.queryAllByTestId("channel-origin")).toHaveLength(0);
    });

    /// Non-web origins (Signal / email / voice / sms) DO surface the
    /// icon, including on user messages — even though web user
    /// messages drop the meta line entirely, an inbound Signal message
    /// keeps it so the operator can see which transport delivered it.
    it("respects an explicit channel_origin field on the payload", () => {
        // The store accepts arbitrary fields; the SPA reads channel_origin
        // off the message object opportunistically.
        appendMessage("conv-2", {
            ...baseMsg(1, "hi"),
            channel_origin: "signal",
        } as never);
        render(<MessageStream conversationId="conv-2" />);
        const origin = screen.getByTestId("channel-origin");
        // The new ChannelIcon component stamps `data-channel` so the
        // SPA tests pin transport-specific rendering by attribute.
        expect(origin).toHaveAttribute("data-channel", "signal");
        // Signal-origin messages must render the BRAND Signal logo
        // (svg), not a generic bi-* icon. The component picks an
        // <svg> element with the official Signal blue when the
        // channel is signal.
        expect(origin.tagName.toLowerCase()).toBe("svg");
    });

    it("renders WhatsApp messages with the WhatsApp bubble styling", () => {
        appendMessage("conv-whatsapp", {
            ...baseMsg(1, "hello from WhatsApp"),
            channel_origin: "whatsapp",
        } as never);
        render(<MessageStream conversationId="conv-whatsapp" />);
        expect(
            document.querySelector(".execlaw-msg__bubble.is-whatsapp"),
        ).toBeTruthy();
        expect(screen.getByTestId("channel-origin")).toHaveAttribute(
            "data-channel",
            "whatsapp",
        );
    });

    it("renders WhatsApp sender context above the message", () => {
        appendMessage("conv-whatsapp-context", {
            ...baseMsg(1, "hello from Jovan"),
            channel_origin: "whatsapp",
            transport_context: "Jovan · +38267123456 · CamperMontenegro",
        } as never);
        render(<MessageStream conversationId="conv-whatsapp-context" />);
        expect(
            document.querySelector(".execlaw-msg__transport-context"),
        ).toHaveTextContent("Jovan · +38267123456 · CamperMontenegro");
    });

    it("renders agent replies in cyan and offers a force action for quiet replies", () => {
        const onForce = vi.fn().mockResolvedValue(undefined);
        appendMessage("conv-whatsapp-quiet", {
            ...baseMsg(1, "I'll stay quiet here since this message isn't directed at me.", "model_turn"),
            channel_origin: "whatsapp",
        } as never);
        render(
            <MessageStream
                conversationId="conv-whatsapp-quiet"
                onForceTransportResponse={onForce}
            />,
        );

        expect(
            document.querySelector(".execlaw-msg__bubble.is-agent.is-whatsapp"),
        ).toBeTruthy();
        expect(screen.getByTestId("force-transport-response")).toBeInTheDocument();
        fireEvent.click(screen.getByTestId("force-transport-response"));
        expect(onForce).toHaveBeenCalledWith(1);
    });

    it("offers a force action for an empty model response", () => {
        const onForce = vi.fn().mockResolvedValue(undefined);
        appendMessage("conv-whatsapp-empty", {
            ...baseMsg(1, "[Empty response]", "model_turn"),
            channel_origin: "whatsapp",
        } as never);
        render(
            <MessageStream
                conversationId="conv-whatsapp-empty"
                onForceTransportResponse={onForce}
            />,
        );
        expect(screen.getByTestId("force-transport-response")).toBeInTheDocument();
    });

    it("offers send and cancel actions for the latest WhatsApp reply", async () => {
        const onSend = vi.fn().mockResolvedValue(undefined);
        appendMessage("conv-whatsapp-review", {
            ...baseMsg(1, "draft reply", "model_turn"),
            channel_origin: "whatsapp",
        } as never);
        render(
            <MessageStream
                conversationId="conv-whatsapp-review"
                onSendTransportReply={onSend}
            />,
        );

        expect(screen.getByTestId("send-transport-reply")).toBeInTheDocument();
        expect(screen.getByTestId("cancel-transport-reply")).toBeInTheDocument();

        fireEvent.click(screen.getByTestId("cancel-transport-reply"));
        expect(screen.queryByTestId("send-transport-reply")).toBeNull();
        expect(screen.queryByTestId("cancel-transport-reply")).toBeNull();
        expect(onSend).not.toHaveBeenCalled();
    });

    it("offers review actions for a Signal-originated latest reply", () => {
        setMessages("conv-signal-origin", [
            {
                ...baseMsg(1, "Signal inbound", "user_msg"),
                channel_origin: "signal",
            },
            {
                ...baseMsg(2, "Signal draft", "model_turn"),
                channel_origin: "signal",
            },
        ] as never);
        render(
            <MessageStream
                conversationId="conv-signal-origin"
                onSendTransportReply={vi.fn().mockResolvedValue(undefined)}
            />,
        );
        expect(screen.getByTestId("send-transport-reply")).toBeInTheDocument();
        expect(screen.getByTestId("cancel-transport-reply")).toBeInTheDocument();
    });

    it("offers independent approval controls for multiple WhatsApp replies", () => {
        setMessages("conv-many-whatsapp-replies", [
            {
                ...baseMsg(1, "first inbound", "user_msg"),
                channel_origin: "whatsapp",
            },
            {
                ...baseMsg(2, "first draft", "model_turn"),
                channel_origin: "whatsapp",
            },
            {
                ...baseMsg(3, "second inbound", "user_msg"),
                channel_origin: "whatsapp",
            },
            {
                ...baseMsg(4, "second draft", "model_turn"),
                channel_origin: "whatsapp",
            },
        ] as never);
        render(
            <MessageStream
                conversationId="conv-many-whatsapp-replies"
                onSendTransportReply={vi.fn().mockResolvedValue(undefined)}
            />,
        );
        expect(screen.getAllByTestId("send-transport-reply")).toHaveLength(2);
        expect(screen.getAllByTestId("cancel-transport-reply")).toHaveLength(2);
    });

    it("uses bootstrap-icons for non-Signal transports", () => {
        // Email / voice / sms ride on `bi-*` since their generic
        // glyphs communicate the channel without needing a brand
        // SVG. The component renders a `<i class="bi …">` for these
        // — same DOM shape as before the Signal logo upgrade.
        appendMessage("conv-email", {
            ...baseMsg(1, "hi"),
            channel_origin: "email",
        } as never);
        render(<MessageStream conversationId="conv-email" />);
        const origin = screen.getByTestId("channel-origin");
        expect(origin).toHaveAttribute("data-channel", "email");
        expect(origin.tagName.toLowerCase()).toBe("i");
        expect(origin.className).toContain("bi-envelope");
    });

    /// 2026-04-28 — when the runner stamps `actor: "agent"` on a
    /// model_turn event, the meta line should read "agent" (not
    /// "agent · agent"). The collapse rule is: skip the actor suffix
    /// when it matches the role.
    it("collapses redundant 'agent · agent' on model_turn meta line", () => {
        setMessages("conv-3", [
            { ...baseMsg(1, "hello world", "model_turn"), actor: "agent" },
        ]);
        render(<MessageStream conversationId="conv-3" />);
        const stream = screen.getByTestId("message-stream");
        // Exactly one "agent" — no duplicate.
        const matches = stream.textContent?.match(/agent/g) ?? [];
        expect(matches.length).toBe(1);
        expect(stream.textContent).not.toContain("agent · agent");
    });

    it("shows timestamp metadata for web-origin user messages", () => {
        setMessages("conv-4", [{ ...baseMsg(1, "hello there"), committed_at: 1_700_000_000 }]);
        render(<MessageStream conversationId="conv-4" />);
        const meta = document.querySelector(".execlaw-msg__meta");
        expect(meta).toBeTruthy();
        expect(meta?.textContent).toContain("you");
        expect(meta?.textContent).toMatch(/\d{4}|\d{2}:\d{2}/);
        expect(
            document.querySelector(".execlaw-msg__bubble.is-user"),
        ).toBeTruthy();
    });

    it("renders long messages in full — no clamp, no Read-more affordance", () => {
        // The Phase-6 truncation spec was reverted 2026-05-15. Long
        // agent replies and drafted long-form content are exactly what
        // the operator wants to read inline; hiding them behind a
        // click was friction. Pin the new contract: the full text
        // reaches the DOM and no Read-more button is ever rendered.
        const longText = Array.from(
            { length: 30 },
            (_, i) => `line-${i + 1}`,
        ).join("\n");
        setMessages("conv-long", [baseMsg(1, longText)]);
        render(<MessageStream conversationId="conv-long" />);
        // No truncation marker / button.
        expect(screen.queryByTestId("msg-truncated")).toBeNull();
        expect(screen.queryByTestId("msg-read-more")).toBeNull();
        // Both ends of the text are present in the DOM — proves we're
        // not silently slicing.
        expect(screen.getByText(/line-1\b/)).toBeInTheDocument();
        expect(screen.getByText(/line-30\b/)).toBeInTheDocument();
    });

    it("doesn't render Read more on short messages either", () => {
        setMessages("conv-short", [baseMsg(1, "just a tiny note")]);
        render(<MessageStream conversationId="conv-short" />);
        expect(screen.queryByTestId("msg-read-more")).toBeNull();
    });

    it("streaming bubble renders with a typing cursor", () => {
        setMessages("conv-stream", []);
        appendStreamingToken("conv-stream", "thinking…");
        render(<MessageStream conversationId="conv-stream" />);
        const bubble = screen.getByTestId("streaming-bubble");
        expect(bubble).toHaveTextContent("thinking…");
        // The blinking cursor is just decorative; assert its presence
        // via the meta block.
        expect(bubble).toHaveTextContent("agent · streaming");
    });

    // ---- scroll-to-bottom button (2026-04-28) -----------------------
    // jsdom doesn't run layout, so `scrollHeight` / `clientHeight`
    // are 0 by default. We patch them per-test to simulate the
    // operator's scroll position and drive the floating ↓ button's
    // visibility + click contract.

    /// Small helper: stamp scroll-geometry properties on a node so
    /// the component's `onScroll` math reflects the simulated layout.
    function setScroll(
        el: HTMLElement,
        { scrollTop, scrollHeight, clientHeight }: {
            scrollTop: number;
            scrollHeight: number;
            clientHeight: number;
        },
    ) {
        // `writable: true` so the component's autoscroll path
        // (`el.scrollTop = el.scrollHeight`) can still run without
        // tripping the read-only property error in jsdom.
        Object.defineProperty(el, "scrollTop", {
            configurable: true,
            writable: true,
            value: scrollTop,
        });
        Object.defineProperty(el, "scrollHeight", {
            configurable: true,
            writable: true,
            value: scrollHeight,
        });
        Object.defineProperty(el, "clientHeight", {
            configurable: true,
            writable: true,
            value: clientHeight,
        });
    }

    it("doesn't show the ↓ button when the operator is at the bottom", () => {
        setMessages("conv-bot", [
            baseMsg(1, "hello"),
            baseMsg(2, "world", "model_turn"),
        ]);
        render(<MessageStream conversationId="conv-bot" />);
        // Initial state: at-bottom — button hidden.
        expect(screen.queryByTestId("scroll-to-bottom")).toBeNull();
    });

    it("surfaces the ↓ button after the operator scrolls up", () => {
        setMessages("conv-up", [
            baseMsg(1, "first"),
            baseMsg(2, "second", "model_turn"),
        ]);
        render(<MessageStream conversationId="conv-up" />);
        const stream = screen.getByTestId("message-stream");
        // Simulate "scrolled up by 200 px from a 1000-px tall content
        // window inside a 400-px viewport". distanceFromBottom = 400.
        setScroll(stream, {
            scrollTop: 200,
            scrollHeight: 1000,
            clientHeight: 400,
        });
        fireEvent.scroll(stream);
        const btn = screen.getByTestId("scroll-to-bottom");
        expect(btn).toBeInTheDocument();
        expect(btn).toHaveAttribute("aria-label", "Scroll to latest message");
    });

    /// 2026-05-04 regression: cards rendered in MessageStream
    /// (research card, attachment chip, etc.) used to anchor to
    /// the outer scroll surface and run the full viewport width —
    /// out of alignment with the surrounding chat bubbles. Each
    /// card is now wrapped in `.execlaw-msg .execlaw-msg--card`
    /// so it inherits the centered + clamped reading-column
    /// treatment messages get via MessageBubble.
    it("wraps each card in .execlaw-msg so it shares the chat-thread reading column", async () => {
        // Side-effect import the AttachmentCard renderer (its
        // module-level registerCardRenderer call is what wires
        // it into the registry). MessageStream's own imports
        // include the LongRunningTaskCard fallback but not the
        // per-kind ones — they're side-effect-imported by Chat.tsx
        // in production. Pulling AttachmentCard in here makes the
        // test self-contained.
        await import("../cards/AttachmentCard");
        const { applyCardEvent } = await import("../cards/cardStore");
        // Seed at least one message so MessageStream renders the
        // list (it short-circuits to a loading state when
        // `messages` is null). The card itself is what we're
        // asserting on — the message just keeps the stream live.
        setMessages("conv-card-margin", [baseMsg(1, "hi")]);
        // Open + close a tiny attachment card on the test
        // conversation so MessageStream renders it inline. Event
        // shapes mirror what the WS bus delivers (see
        // crates/server/src/events.rs).
        applyCardEvent("conv-card-margin", {
            kind: "card.opened",
            payload: {
                card_id: "card-1",
                kind: "attachment",
                title: "report.pdf",
                summary: "report.pdf (application/pdf)",
                state: "Running",
                details: {
                    attachment_id: "att-1",
                    filename: "report.pdf",
                    mime_type: "application/pdf",
                    download_url: "/api/attachments/att-1",
                },
                actions: [],
            },
            committed_at: 1,
            event_seq: 1,
        });
        applyCardEvent("conv-card-margin", {
            kind: "card.closed",
            payload: {
                card_id: "card-1",
                state: "Completed",
                summary: "report.pdf (application/pdf)",
                details: {
                    attachment_id: "att-1",
                    filename: "report.pdf",
                    mime_type: "application/pdf",
                    download_url: "/api/attachments/att-1",
                },
                attachment_id: "att-1",
                error: undefined,
            },
            committed_at: 2,
            event_seq: 2,
        });

        render(<MessageStream conversationId="conv-card-margin" />);
        const chip = screen.getByTestId("card-attachment");
        // Walk up to find the wrapper. Must be a direct (or near-
        // direct) ancestor with .execlaw-msg — otherwise the
        // shared margin/centering CSS doesn't apply.
        let cursor: HTMLElement | null = chip;
        let foundWrapper = false;
        while (cursor) {
            if (cursor.classList.contains("execlaw-msg")) {
                foundWrapper = true;
                break;
            }
            cursor = cursor.parentElement;
        }
        expect(foundWrapper).toBe(true);
    });

    it("clicking the ↓ button calls scrollTo and the button hides on next scroll event", () => {
        setMessages("conv-click", [
            baseMsg(1, "first"),
            baseMsg(2, "second", "model_turn"),
        ]);
        render(<MessageStream conversationId="conv-click" />);
        const stream = screen.getByTestId("message-stream");
        // Stub scrollTo so the click handler doesn't blow up in jsdom.
        const scrollToSpy = vi.fn();
        Object.defineProperty(stream, "scrollTo", {
            configurable: true,
            value: scrollToSpy,
        });
        // Scroll up far enough to surface the button.
        setScroll(stream, {
            scrollTop: 0,
            scrollHeight: 800,
            clientHeight: 400,
        });
        fireEvent.scroll(stream);
        const btn = screen.getByTestId("scroll-to-bottom");

        fireEvent.click(btn);
        expect(scrollToSpy).toHaveBeenCalledWith({
            top: 800,
            behavior: "smooth",
        });

        // After the click, simulate the resulting scroll landing at
        // bottom and emitting a scroll event. The button should
        // unmount.
        setScroll(stream, {
            scrollTop: 400,
            scrollHeight: 800,
            clientHeight: 400,
        });
        fireEvent.scroll(stream);
        expect(screen.queryByTestId("scroll-to-bottom")).toBeNull();
    });

    // ---- skill chip + prepend stripping (composer `+` menu) -----

    /// When the operator picked skills via the composer's `+` menu,
    /// the server prepends `<skill name="...">...</skill>` blocks
    /// onto the user_msg text. The bubble must:
    ///   * render only the original user text (blocks stripped), AND
    ///   * surface the applied skill names as a chip below the
    ///     bubble so the operator can see what shaped the response.
    it("strips the prepended skill block from the bubble and renders chips", () => {
        const prepended =
            '<skill name="test/foo">\n' +
            "Always answer in haiku.\n" +
            "</skill>\n\n" +
            "tell me a story";
        appendMessage("conv-skill", {
            ...baseMsg(1, prepended),
            applied_skill_names: ["test/foo"],
        } as never);
        render(<MessageStream conversationId="conv-skill" />);
        // Original user text reaches the bubble.
        expect(screen.getByText(/tell me a story/)).toBeInTheDocument();
        // Prepended block does NOT.
        expect(screen.queryByText(/Always answer in haiku/)).toBeNull();
        // Chip surfaces the applied skill name.
        const chips = screen.getAllByTestId("message-applied-skill");
        expect(chips).toHaveLength(1);
        expect(chips[0].getAttribute("data-skill-name")).toBe("test/foo");
    });

    /// Multi-skill: every leading block strips; chips for all names
    /// render in order.
    it("strips multiple consecutive skill blocks and renders chips in order", () => {
        const prepended =
            '<skill name="test/alpha">\n' +
            "alpha guidance\n" +
            "</skill>\n\n" +
            '<skill name="test/beta">\n' +
            "beta guidance\n" +
            "</skill>\n\n" +
            "do the thing";
        appendMessage("conv-multi", {
            ...baseMsg(1, prepended),
            applied_skill_names: ["test/alpha", "test/beta"],
        } as never);
        render(<MessageStream conversationId="conv-multi" />);
        expect(screen.getByText(/do the thing/)).toBeInTheDocument();
        expect(screen.queryByText(/alpha guidance/)).toBeNull();
        expect(screen.queryByText(/beta guidance/)).toBeNull();
        const chips = screen.getAllByTestId("message-applied-skill");
        expect(chips.map((c) => c.getAttribute("data-skill-name"))).toEqual([
            "test/alpha",
            "test/beta",
        ]);
    });

    /// Regression for "user typed text that looks like a skill block":
    /// without `applied_skill_names` set, the bubble renders the raw
    /// text verbatim. Stripping must be gated on the field being
    /// non-empty so we never trim user content.
    it("does NOT strip skill-shaped text when applied_skill_names is empty", () => {
        const literal =
            '<skill name="example/bad">should stay</skill>\n\nhi there';
        setMessages("conv-no-skills", [baseMsg(1, literal)]);
        render(<MessageStream conversationId="conv-no-skills" />);
        // The literal user text is preserved (markdown renders the
        // angle brackets as literal text since no html mode is on).
        const stream = screen.getByTestId("message-stream");
        expect(stream.textContent).toContain("should stay");
        expect(screen.queryByTestId("message-applied-skills")).toBeNull();
    });
});

describe("stripSkillPrependBlock", () => {
    it("returns the original text unchanged when no skill block is present", () => {
        expect(stripSkillPrependBlock("hello world")).toBe("hello world");
    });

    it("strips a single block plus the blank line separator", () => {
        const t = '<skill name="ns/x">\nbody\n</skill>\n\nuser text';
        expect(stripSkillPrependBlock(t)).toBe("user text");
    });

    it("strips multiple consecutive blocks", () => {
        const t =
            '<skill name="ns/a">a</skill>\n\n' +
            '<skill name="ns/b">b</skill>\n\n' +
            "go";
        expect(stripSkillPrependBlock(t)).toBe("go");
    });

    it("only strips leading blocks, not blocks embedded in user text", () => {
        const t = 'plain text\n\n<skill name="ns/x">should stay</skill>';
        // No leading block → nothing stripped.
        expect(stripSkillPrependBlock(t)).toBe(t);
    });
});

describe("formatMessageTimestamp", () => {
    it("returns empty string for invalid timestamps", () => {
        expect(formatMessageTimestamp(0)).toBe("");
        expect(formatMessageTimestamp(Number.NaN)).toBe("");
    });

    it("formats valid unix-seconds as local date/time", () => {
        const s = formatMessageTimestamp(1_700_000_000);
        expect(s.length).toBeGreaterThan(0);
    });
});
