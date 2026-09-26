// Vertically scrolling message list for the active thread.
//
// Auto-scrolls to the latest message on append IF the operator is
// already at the bottom. When the operator has scrolled up to read
// history, incoming messages no longer yank the viewport — instead
// a small ↓ button surfaces above the composer; clicking it snaps
// to the latest. Long messages render in full — no clamp, no
// "Read more…" affordance (the Phase-6 truncation spec was reverted
// 2026-05-15: in practice, a click-to-expand hides important
// context behind a friction the operator never asked for, and the
// drafted long-form replies the agent produces are exactly the
// content the operator wants to read inline). Each bubble also
// renders a subtle channel-origin icon (signal / email / voice) so
// the controller can see at a glance which transport delivered the
// message.

import {
    Fragment,
    useCallback,
    useContext,
    useEffect,
    useMemo,
    useRef,
    useState,
} from "react";
import {
    deleteNexusView, getNexusOrganization, listMessages, saveNexusAnnotation,
    saveNexusView, searchNexusMessages,
    type AvailableTransportView, type MessageView, type NexusAnnotation,
    type NexusLink, type NexusOrganization, type NexusSearchHit,
} from "../api/endpoints";
import { signDownloadUrl } from "../api/signedDownloadUrl";
import { AuthContext } from "../auth/AuthContext";
import { getCardRenderer } from "../cards/CardRenderer";
import { useCardsForConversation } from "../cards/cardStore";
import type { Card } from "../cards/types";
import { ChannelIcon } from "../components/ChannelIcons";
import { MarkdownContent } from "../components/MarkdownContent";
import { ToolActivityPill } from "./ToolActivityPill";
import {
    detectChatComponent,
    getChatComponent,
} from "./chatComponentRegistry";
// Side-effect imports — each module self-registers its renderer with
// the chat-component registry at module-load. Listed here so the
// bundler keeps them in the SPA build; without this they'd be tree-
// shaken since no symbol from them is referenced directly.
import "./components/ChartInlineComponent";
import "./components/WeatherCurrentComponent";
import "./components/WeatherDailyComponent";
import "./components/PythonExecuteComponent";
import { mergeMessages, useChatState } from "./store";
import { useChatAppearance } from "./useChatAppearance";
import { useT } from "../i18n";

// 2026-05-18 — helpers for the file-vs-image branching when
// rendering attachments under user message bubbles. Image MIMEs
// mirror the server's `ALLOWED_ATTACHMENT_MIMES` image subset;
// anything else takes the file-chip path.
const IMAGE_MIME_SET: ReadonlySet<string> = new Set([
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
]);
function isImageMime(m: string): boolean {
    return IMAGE_MIME_SET.has(m);
}

/// Bootstrap-icons class for a non-image attachment chip. Mirrors
/// the Composer's `fileIconForMime` so a CSV looks the same in the
/// composer preview and the rendered message.
function fileIconForMime(mime: string): string {
    if (mime === "application/pdf") return "bi bi-file-earmark-pdf";
    if (mime === "text/csv" || mime === "text/tab-separated-values")
        return "bi bi-file-earmark-spreadsheet";
    if (
        mime ===
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" ||
        mime === "application/vnd.ms-excel"
    )
        return "bi bi-file-earmark-excel";
    if (mime === "application/json") return "bi bi-file-earmark-code";
    if (mime === "text/markdown") return "bi bi-markdown";
    if (mime === "text/plain") return "bi bi-file-earmark-text";
    return "bi bi-file-earmark";
}

/// Compact byte-size formatter for the message-attachment chip.
/// Matches the Composer's `formatBytes` style ("5.2 KB", "1.4 MB").
function formatBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
    return `${(n / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

interface Props {
    conversationId: string;
    availableTransports?: AvailableTransportView[];
    onSendTransportReply?: (
        text: string,
        sourceSeq: number,
        channel: string,
    ) => Promise<void>;
    onForceTransportResponse?: (sourceSeq: number) => Promise<void>;
    onRerunResponse?: (sourceSeq: number) => Promise<void>;
    onSetTransportReviewDecision?: (
        sourceSeq: number,
        decision: "cancelled" | "pending",
    ) => Promise<void>;
    /**
     * 2026-05-16 — when false, `tool_use` / `tool_result` messages
     * are filtered out of the stream entirely (not just hidden via
     * CSS — dropped from the items list so the DOM stays small).
     * Default `true` preserves historical behaviour for tests + any
     * caller that doesn't thread the toggle. The operator flips
     * this via the chat header's view-filter popup; the preference
     * persists across reloads (see `useToolResultsVisible`).
     */
    showToolResults?: boolean;
}

/** Pixel slack on the at-bottom check. Browsers can leave a fractional
 *  gap between scrollTop+clientHeight and scrollHeight even when the
 *  user is visually at the floor; 8 px swallows that noise. */
const AT_BOTTOM_SLACK_PX = 8;

/// Discriminated union the chronological-render loop walks. Cards
/// and messages are interleaved by their wall-clock timestamp.
type StreamItem =
    | { kind: "message"; message: MessageView; sortKey: number }
    | { kind: "card"; card: Card; sortKey: number };

export function MessageStream({
    conversationId,
    availableTransports = [],
    showToolResults = true,
    onSendTransportReply,
    onForceTransportResponse,
    onRerunResponse,
    onSetTransportReviewDecision,
}: Props) {
    const [appearance] = useChatAppearance();
    const nexus = appearance === "nexus";
    const t = useT();
    const auth = useContext(AuthContext);
    const getToken = auth?.getAccessToken;
    const [selectedSource, setSelectedSource] = useState("");
    const [sourceOrder, setSourceOrder] = useState<"first" | "name">("first");
    const [searchText, setSearchText] = useState("");
    const [view, setView] = useState<"conversation" | "relationships">("conversation");
    const [selectedTransportBySeq, setSelectedTransportBySeq] = useState<Record<number, string>>({});
    const [activeSeq, setActiveSeq] = useState<number | null>(null);
    const [collapsedGroups, setCollapsedGroups] = useState<Record<string, boolean>>({});
    const [collapsedBranches, setCollapsedBranches] = useState<Record<string, boolean>>({});
    const [organization, setOrganization] = useState<NexusOrganization>({ annotations: [], views: [] });
    const [selectedTag, setSelectedTag] = useState("");
    const [selectedBranch, setSelectedBranch] = useState("");
    const [selectedKind, setSelectedKind] = useState("");
    const [viewName, setViewName] = useState("");
    const [searchHits, setSearchHits] = useState<NexusSearchHit[]>([]);
    const [searchAttempted, setSearchAttempted] = useState(false);
    const [searchMore, setSearchMore] = useState(false);
    const [searchError, setSearchError] = useState("");
    const searchRequestId = useRef(0);
    const [buffered, setBuffered] = useState<Record<string, number>>({});
    const [newMessageSeqs, setNewMessageSeqs] = useState<number[]>([]);
    const lastSeenSeq = useRef<number | null>(null);
    const messages = useChatState(
        (s) => s.messages[conversationId] ?? null,
    );
    const streaming = useChatState(
        (s) => s.streamingBuffer[conversationId] ?? null,
    );
    const cards = useCardsForConversation(conversationId);
    const items: StreamItem[] = useMemo(() => {
        const acc: StreamItem[] = [];
        if (messages) {
            for (const m of messages) {
                // 2026-05-16 — tool_use / tool_result bubbles are
                // typically raw JSON / monospace dumps that crowd
                // the transcript when the operator just wants the
                // agent's prose. The chat header's view filter
                // gates them on/off; default is on.
                if (!showToolResults && isToolKind(m.kind)) continue;
                acc.push({ kind: "message", message: m, sortKey: m.seq });
            }
        }
        // 2026-05-03 — cards now arrive with their real
        // `state_events.seq` from the WS payload (`event_seq`).
        // Use it as the sortKey so cards land inline at their
        // true position in the chat-thread timeline, alongside
        // messages, instead of being pinned to the tail.
        //
        // Fallback for legacy WS payloads / fixtures with no seq:
        // synthesise a key from `opened_at` × 1e6 so the card
        // still lands chronologically (and at the bottom for
        // `event_seq`-less cards opened in the past, matching
        // the prior behavior).
        for (const c of cards) {
            const sortKey =
                c.event_seq !== null && c.event_seq !== undefined
                    ? c.event_seq
                    : c.opened_at * 1_000_000;
            acc.push({
                kind: "card",
                card: c,
                sortKey,
            });
        }
        acc.sort((a, b) => a.sortKey - b.sortKey);
        return acc;
    }, [messages, cards, showToolResults]);
    const scrollRef = useRef<HTMLDivElement | null>(null);
    const visibleMessages = items.flatMap((item) => item.kind === "message" ? [item.message] : []);
    const annotations = new Map(organization.annotations.map((annotation) => [annotation.seq, annotation]));
    const branches = [...new Set(organization.annotations.map((annotation) => annotation.branch_id).filter((branch): branch is string => !!branch))];
    const branchLatestSeq = new Map<string, number>();
    for (const message of visibleMessages) {
        const branch = annotations.get(message.seq)?.branch_id;
        if (branch) branchLatestSeq.set(branch, message.seq);
    }
    const tags = [...new Set(organization.annotations.flatMap((annotation) => annotation.tags))].sort();
    const sources = [...new Set(visibleMessages.map(messageSource))];
    const orderedSources = sourceOrder === "name" ? [...sources].sort((left, right) => left.localeCompare(right)) : sources;
    const effectiveSource = sources.includes(selectedSource) ? selectedSource : "";
    const matchingMessages = visibleMessages.filter((message) =>
        (!effectiveSource || messageSource(message) === effectiveSource) &&
        (!searchText.trim() || (message.text ?? "").toLocaleLowerCase().includes(searchText.trim().toLocaleLowerCase())) &&
        (!selectedTag || annotations.get(message.seq)?.tags.includes(selectedTag)) &&
        (!selectedBranch || annotations.get(message.seq)?.branch_id === selectedBranch) &&
        (!selectedKind || message.kind === selectedKind)
    );
    const sourceMessages = new Map(visibleMessages.map((message) => [message.seq, message]));
    const groupKeys = new Map<number, string>();
    let lastGroup: string | null = null;
    let groupKey = "";
    for (const message of visibleMessages) {
        const group = message.transport_group ?? null;
        if (group && group !== lastGroup) groupKey = `${group}:${message.seq}`;
        if (group) groupKeys.set(message.seq, groupKey);
        lastGroup = group;
    }
    const pendingJump = useRef<number | null>(null);
    const jumpToMessage = (seq: number) => {
        const branch = annotations.get(seq)?.branch_id;
        if (branch && collapsedBranches[branch] && branchLatestSeq.get(branch) !== seq) {
            pendingJump.current = seq;
            setCollapsedBranches((current) => ({ ...current, [branch]: false }));
            return;
        }
        const targetGroupKey = groupKeys.get(seq);
        if (targetGroupKey && collapsedGroups[targetGroupKey]) {
            pendingJump.current = seq;
            setCollapsedGroups((current) => ({ ...current, [targetGroupKey]: false }));
            return;
        }
        const element = scrollRef.current?.querySelector<HTMLElement>(`[data-message-seq="${seq}"]`);
        if (!element) return;
        setIsAtBottom(false);
        setActiveSeq(seq);
        element.scrollIntoView({ behavior: "auto", block: "center" });
        element.focus({ preventScroll: true });
    };
    useEffect(() => {
        if (pendingJump.current != null) {
            const seq = pendingJump.current;
            pendingJump.current = null;
            jumpToMessage(seq);
        }
    }, [collapsedGroups, collapsedBranches, messages]);
    const navigateSource = (direction: number) => {
        if (!matchingMessages.length) return;
        const current = matchingMessages.findIndex((message) => message.seq === activeSeq);
        const next = current < 0
            ? direction > 0 ? 0 : matchingMessages.length - 1
            : (current + direction + matchingMessages.length) % matchingMessages.length;
        jumpToMessage(matchingMessages[next].seq);
    };
    const [isAtBottom, setIsAtBottom] = useState(true);
    const [sendingReplySeq, setSendingReplySeq] = useState<number | null>(null);
    const [rerunningSeq, setRerunningSeq] = useState<number | null>(null);
    const [optimisticReviewStates, setOptimisticReviewStates] = useState<
        Record<number, "sent" | "cancelled">
    >({});

    useEffect(() => {
        if (!nexus || !getToken || messages === null) return;
        let current = true;
        getNexusOrganization(conversationId, getToken)
            .then((value) => { if (current) setOrganization(value); })
            .catch(() => { if (current) setSearchError("Organization unavailable"); });
        return () => { current = false; };
    }, [conversationId, nexus, getToken, messages === null]);

    useEffect(() => {
        const maxSeq = visibleMessages.reduce((max, message) => Math.max(max, message.seq), 0);
        if (lastSeenSeq.current != null && !isAtBottom) {
            const arrivals = visibleMessages.filter((message) => message.seq > lastSeenSeq.current!);
            if (arrivals.length) setBuffered((previous) => {
                const next = { ...previous };
                for (const message of arrivals) {
                    const source = messageSource(message);
                    next[source] = (next[source] ?? 0) + 1;
                }
                return next;
            });
            if (arrivals.length) setNewMessageSeqs((previous) => [...new Set([...previous, ...arrivals.map((message) => message.seq)])]);
        }
        lastSeenSeq.current = Math.max(lastSeenSeq.current ?? 0, maxSeq);
    }, [messages, isAtBottom]);

    const saveAnnotation = async (annotation: NexusAnnotation) => {
        if (!getToken) return;
        await saveNexusAnnotation(conversationId, annotation, getToken);
        setOrganization((current) => ({
            ...current,
            annotations: [...current.annotations.filter((item) => item.seq !== annotation.seq), annotation],
        }));
    };

    const runSearch = async (before?: number) => {
        if (!getToken || searchText.trim().length < 2) return;
        const requestId = ++searchRequestId.current;
        try {
            setSearchError("");
            const result = await searchNexusMessages(conversationId, searchText.trim(), getToken, before, effectiveSource ? effectiveSource.toLowerCase() : undefined);
            if (requestId !== searchRequestId.current) return;
            setSearchHits((previous) => before == null ? result.matches : [...previous, ...result.matches]);
            setSearchAttempted(true);
            setSearchMore(result.has_more);
        } catch {
            if (requestId === searchRequestId.current) setSearchError("Search unavailable");
        }
    };

    const jumpToSearchHit = async (seq: number) => {
        if (!sourceMessages.has(seq) && getToken) {
            try {
                const window = await listMessages(conversationId, getToken, { around: seq, limit: 100 });
                pendingJump.current = seq;
                mergeMessages(conversationId, window.messages);
            } catch {
                setSearchError("Message unavailable");
            }
        } else {
            jumpToMessage(seq);
        }
    };

    // Auto-stick to the bottom only when the operator is already
    // there. Mid-history scroll-up means "I'm reading older content,
    // don't yank me away" — incoming messages just pile under the
    // viewport and the ↓ button surfaces.
    useEffect(() => {
        const el = scrollRef.current;
        if (!el) return;
        if (isAtBottom) {
            el.scrollTop = el.scrollHeight;
        }
    }, [messages, streaming, isAtBottom]);

    const onScroll = useCallback(() => {
        const el = scrollRef.current;
        if (!el) return;
        const distanceFromBottom =
            el.scrollHeight - el.scrollTop - el.clientHeight;
        const atBottom = distanceFromBottom <= AT_BOTTOM_SLACK_PX;
        setIsAtBottom(atBottom);
        if (atBottom) { setBuffered({}); setNewMessageSeqs([]); }
    }, []);

    const scrollToBottom = useCallback(() => {
        const el = scrollRef.current;
        if (!el) return;
        el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
        setBuffered({});
        setNewMessageSeqs([]);
    }, []);

    // Re-establish at-bottom on conversation switch so the new
    // thread starts in autoscroll mode.
    useEffect(() => {
        setIsAtBottom(true);
        setSelectedSource("");
        setSourceOrder("first");
        setSearchText("");
        setView("conversation");
        setActiveSeq(null);
        setCollapsedGroups({});
        setCollapsedBranches({});
        setOrganization({ annotations: [], views: [] });
        setSelectedTag("");
        setSelectedBranch("");
        setSelectedKind("");
        setSearchHits([]);
        setSearchAttempted(false);
        setViewName("");
        searchRequestId.current++;
        setSearchError("");
        setBuffered({});
        setNewMessageSeqs([]);
        lastSeenSeq.current = null;
    }, [conversationId]);

    if (messages === null) {
        return (
            <div className="execlaw-stream-wrap">
                <div className="execlaw-stream" ref={scrollRef}>
                    <div className="execlaw-empty-state small">
                        Loading messages…
                    </div>
                </div>
            </div>
        );
    }

    if (messages.length === 0 && cards.length === 0 && !streaming) {
        return (
            <div className="execlaw-stream-wrap">
                <div className="execlaw-stream" ref={scrollRef}>
                    <div className="execlaw-empty-state">
                        <i
                            className="bi bi-chat-square-dots"
                            style={{
                                fontSize: "2rem",
                                display: "block",
                                marginBottom: "0.5rem",
                            }}
                            aria-hidden
                        />
                        No messages yet. Type below to start.
                    </div>
                </div>
            </div>
        );
    }

    return (
        <div className={`execlaw-stream-wrap${nexus ? " execlaw-nexus" : ""}`}>
            {nexus && (
                <nav className="execlaw-nexus__navigator" aria-label={t("chat.sourceNavigation", "Message source navigation")}>
                    <div className="execlaw-nexus__views" role="group" aria-label={t("chat.nexusView", "Nexus view")}>
                        {(["conversation", "relationships"] as const).map((mode) => (
                            <button key={mode} type="button" aria-pressed={view === mode}
                                onClick={() => setView(mode)}>
                                <i className={`bi bi-${mode === "conversation" ? "chat-left-text" : "diagram-3"}`} aria-hidden />
                                {mode === "conversation" ? t("chat.conversationView", "Conversation") : t("chat.relationshipView", "Relationships")}
                            </button>
                        ))}
                    </div>
                    <div className="execlaw-nexus__summary">
                        <i className="bi bi-diagram-3" aria-hidden />
                        <strong>{t("chat.sources", "Sources")}</strong>
                        <span>{sources.length}</span>
                    </div>
                    <select
                        className="form-select form-select-sm"
                        aria-label={t("chat.messageSource", "Message source")}
                        value={effectiveSource}
                        onChange={(event) => {
                            setSelectedSource(event.target.value);
                            searchRequestId.current++;
                            setSearchHits([]);
                            setSearchAttempted(false);
                            setActiveSeq(null);
                            setIsAtBottom(false);
                        }}
                    >
                        <option value="">{t("chat.allSources", "All sources")}</option>
                        {orderedSources.map((source) => <option key={source} value={source}>{source}</option>)}
                    </select>
                    <select className="execlaw-nexus__order" aria-label={t("chat.sourceOrder", "Source order")}
                        value={sourceOrder} onChange={(event) => setSourceOrder(event.target.value as "first" | "name")}>
                        <option value="first">{t("chat.firstAppearance", "First seen")}</option>
                        <option value="name">{t("chat.sourceName", "Source name")}</option>
                    </select>
                    <input type="search" className="execlaw-nexus__search"
                        aria-label={t("chat.searchMessages", "Search messages")}
                        placeholder={t("chat.searchMessages", "Search messages")}
                        value={searchText}
                        onChange={(event) => {
                            setSearchText(event.target.value);
                            searchRequestId.current++;
                            setSearchHits([]);
                            setSearchAttempted(false);
                            setActiveSeq(null);
                            setIsAtBottom(false);
                        }} onKeyDown={(event) => { if (event.key === "Enter") void runSearch(); }} />
                    {getToken && <button type="button" className="execlaw-nexus__nav-button"
                        title="Search full history" aria-label="Search full history"
                        disabled={searchText.trim().length < 2} onClick={() => void runSearch()}>
                        <i className="bi bi-search" aria-hidden />
                    </button>}
                    <select className="execlaw-nexus__order" aria-label="Message type"
                        value={selectedKind} onChange={(event) => setSelectedKind(event.target.value)}>
                        <option value="">All types</option>
                        <option value="user_msg">Incoming</option>
                        <option value="model_turn">Responses</option>
                        <option value="tool_use">Tool requests</option>
                        <option value="tool_result">Tool results</option>
                    </select>
                    {tags.length > 0 && <select className="execlaw-nexus__order" aria-label="Filter by tag"
                        value={selectedTag} onChange={(event) => setSelectedTag(event.target.value)}>
                        <option value="">All tags</option>
                        {tags.map((tag) => <option key={tag} value={tag}>{tag}</option>)}
                    </select>}
                    {branches.length > 0 && <select className="execlaw-nexus__order" aria-label="Filter by branch"
                        value={selectedBranch} onChange={(event) => setSelectedBranch(event.target.value)}>
                        <option value="">All branches</option>
                        {branches.map((branch) => <option key={branch} value={branch}>{branch}</option>)}
                    </select>}
                    <span className="execlaw-nexus__count" aria-live="polite">
                        {Math.max(0, matchingMessages.findIndex((message) => message.seq === activeSeq) + 1)} / {matchingMessages.length}
                    </span>
                    {([-1, 1] as const).map((direction) => (
                        <button
                            key={direction}
                            type="button"
                            className="execlaw-nexus__nav-button"
                            disabled={!matchingMessages.length}
                            title={direction < 0 ? t("chat.previousSource", "Previous matching message") : t("chat.nextSource", "Next matching message")}
                            aria-label={direction < 0 ? t("chat.previousSource", "Previous matching message") : t("chat.nextSource", "Next matching message")}
                            onClick={() => navigateSource(direction)}
                        >
                            <i className={`bi bi-arrow-${direction < 0 ? "up" : "down"}`} aria-hidden />
                        </button>
                    ))}
                    {getToken && <div className="execlaw-nexus__saved">
                        <select aria-label="Saved view" value="" onChange={(event) => {
                            const chosen = organization.views.find((item) => item.name === event.target.value);
                            if (!chosen) return;
                            setSelectedSource(chosen.filters.source ?? "");
                            setSelectedTag(chosen.filters.tag ?? "");
                            setSelectedKind(chosen.filters.kind ?? "");
                            setSearchText(chosen.filters.query ?? "");
                            setViewName(chosen.name);
                        }}>
                            <option value="">Saved views</option>
                            {organization.views.map((saved) => <option key={saved.name} value={saved.name}>{saved.name}</option>)}
                        </select>
                        <input aria-label="View name" placeholder="View name" maxLength={48} value={viewName}
                            onChange={(event) => setViewName(event.target.value)} />
                        <button type="button" title="Save view" aria-label="Save view" disabled={!viewName.trim()}
                            onClick={() => void saveNexusView(conversationId, {
                                name: viewName.trim(), filters: {
                                    source: effectiveSource || undefined, tag: selectedTag || undefined,
                                    kind: selectedKind || undefined, query: searchText || undefined,
                                },
                            }, getToken).then(() => getNexusOrganization(conversationId, getToken)).then(setOrganization).catch(() => setSearchError("Unable to save view"))}>
                            <i className="bi bi-bookmark-plus" aria-hidden />
                        </button>
                        {organization.views.some((item) => item.name === viewName.trim()) &&
                            <button type="button" title="Delete view" aria-label="Delete view"
                                onClick={() => void deleteNexusView(conversationId, viewName.trim(), getToken)
                                    .then(() => getNexusOrganization(conversationId, getToken)).then(setOrganization).catch(() => setSearchError("Unable to delete view"))}>
                                <i className="bi bi-trash" aria-hidden />
                            </button>}
                    </div>}
                    {branches.length > 0 && <div className="execlaw-nexus__branches" aria-label="Conversation branches">
                        {branches.map((branch) => {
                            const members = visibleMessages.filter((message) => annotations.get(message.seq)?.branch_id === branch);
                            const latest = members.at(-1);
                            return <div key={branch} className="execlaw-nexus__branch-item">
                                <button type="button" aria-pressed={selectedBranch === branch}
                                    onClick={() => { setSelectedBranch(selectedBranch === branch ? "" : branch); if (latest) jumpToMessage(latest.seq); }}>
                                    <i className="bi bi-diagram-2" aria-hidden /> {branch} · {organization.annotations.filter((item) => item.branch_id === branch).length} messages · {new Set(members.map(messageSource)).size} loaded sources
                                    {latest && <span> · latest #{latest.seq}</span>}
                                    {members.some((message) => newMessageSeqs.includes(message.seq)) &&
                                        <span> · {members.filter((message) => newMessageSeqs.includes(message.seq)).length} new</span>}
                                </button>
                                {members.length > 1 && <button type="button" aria-expanded={!collapsedBranches[branch]}
                                    aria-label={`${collapsedBranches[branch] ? "Expand" : "Collapse"} branch ${branch}`}
                                    onClick={() => setCollapsedBranches((current) => ({ ...current, [branch]: !current[branch] }))}>
                                    <i className={`bi bi-chevron-${collapsedBranches[branch] ? "down" : "up"}`} aria-hidden />
                                </button>}
                            </div>;
                        })}
                    </div>}
                    {(searchAttempted || searchError) && <div className="execlaw-nexus__results" role="region" aria-label="Full history search results">
                        {searchError && <span role="alert">{searchError}</span>}
                        {searchAttempted && searchHits.length === 0 && <span>No matches in this conversation</span>}
                        {searchHits.map((hit) => <button type="button" key={hit.seq}
                            onClick={() => void jumpToSearchHit(hit.seq)}>
                            <span>{hit.source} #{hit.seq} · {formatMessageTimestamp(hit.committed_at)}</span>
                            <span>{hit.text}</span>
                        </button>)}
                        {searchMore && <button type="button" onClick={() => void runSearch(searchHits.at(-1)?.seq)}>Older matches</button>}
                    </div>}
                </nav>
            )}
            <div
                className="execlaw-stream"
                ref={scrollRef}
                onScroll={onScroll}
                data-testid="message-stream"
            >
                {items.map((item) => {
                    if (item.kind === "message") {
                        const m = item.message;
                        const branch = annotations.get(m.seq)?.branch_id;
                        if (branch && collapsedBranches[branch] && branchLatestSeq.get(branch) !== m.seq) return null;
                        const group = nexus ? m.transport_group ?? null : null;
                        const currentGroupKey = groupKeys.get(m.seq);
                        const groupCollapsed = !!currentGroupKey && collapsedGroups[currentGroupKey];
                        const index = items.indexOf(item);
                        const previous = items
                            .slice(0, index)
                            .reverse()
                            .find((candidate) => candidate.kind === "message");
                        const previousGroup = previous?.kind === "message"
                            ? previous.message.transport_group
                            : null;
                        const groupStart = !!group && group !== previousGroup;
                        const next = items.slice(index + 1).find((candidate) => candidate.kind === "message");
                        const groupEnd = !!group && (next?.kind !== "message" || next.message.transport_group !== group);
                        const groupMembers: MessageView[] = [];
                        if (groupStart) {
                            for (const candidate of items.slice(index)) {
                                if (candidate.kind !== "message") continue;
                                if (candidate.message.transport_group !== group) break;
                                groupMembers.push(candidate.message);
                            }
                        }
                        if (groupCollapsed && !groupStart && !groupEnd) return null;
                        return (
                            <Fragment key={`msg-${m.kind}-${m.seq}`}>
                                {groupStart && (
                                    <button
                                        type="button"
                                        className="execlaw-nexus__group-header"
                                        onClick={() => setCollapsedGroups((current) => ({
                                            ...current,
                                            [currentGroupKey!]: !current[currentGroupKey!],
                                        }))}
                                        aria-expanded={!groupCollapsed}
                                    >
                                        <span>
                                            <i className="bi bi-people" aria-hidden />
                                            {group}
                                            {groupMembers.length > 0 && <small>{groupMembers.length} messages · {new Set(groupMembers.map(messageSource)).size} sources</small>}
                                            {groupMembers.some((message) => newMessageSeqs.includes(message.seq)) &&
                                                <small>{groupMembers.filter((message) => newMessageSeqs.includes(message.seq)).length} new</small>}
                                        </span>
                                        <span className="execlaw-nexus__group-toggle">
                                            {groupCollapsed ? "Expand group" : "Collapse group"}
                                            <i className={`bi bi-chevron-${groupCollapsed ? "down" : "up"}`} aria-hidden />
                                        </span>
                                    </button>
                                )}
                            {(!groupCollapsed || groupEnd) && <MessageBubble
                                message={m}
                                nexus={nexus}
                                highlighted={activeSeq === m.seq}
                                sourceMatch={(!!effectiveSource || !!searchText.trim()) && matchingMessages.includes(m)}
                                dimmed={(view === "relationships" && m.reply_to_seq == null && !visibleMessages.some((reply) => reply.reply_to_seq === m.seq)
                                    && !annotations.get(m.seq)?.links.length && !organization.annotations.some((item) => item.links.some((link) => link.target_seq === m.seq)))
                                    || (!!selectedBranch && annotations.get(m.seq)?.branch_id !== selectedBranch)
                                    || (!!selectedTag && !annotations.get(m.seq)?.tags.includes(selectedTag))
                                    || (!!selectedKind && m.kind !== selectedKind)}
                                replySource={m.reply_to_seq == null ? undefined : sourceMessages.get(m.reply_to_seq)}
                                annotation={annotations.get(m.seq)}
                                messageChoices={visibleMessages}
                                onSaveAnnotation={saveAnnotation}
                                onJumpToMessage={jumpToMessage}
                                showTransportSend={
                                    !!readChannelOrigin(m) &&
                                    m.kind === "model_turn" &&
                                    m.review_state !== "cancelled" &&
                                    optimisticReviewStates[m.seq] !== "cancelled" &&
                                    !!onSendTransportReply
                                }
                                transportSendBusy={sendingReplySeq === m.seq}
                                transportChoices={availableTransports}
                                selectedTransport={
                                    selectedTransportBySeq[m.seq] ?? readChannelOrigin(m) ?? ""
                                }
                                onTransportChange={(channel) => {
                                    setSelectedTransportBySeq((current) => ({
                                        ...current,
                                        [m.seq]: channel,
                                    }));
                                }}
                                onSendTransportReply={async () => {
                                    if (!onSendTransportReply) return;
                                    setSendingReplySeq(m.seq);
                                    try {
                                        await onSendTransportReply(
                                            m.text ?? "",
                                            m.seq,
                                            selectedTransportBySeq[m.seq] ?? readChannelOrigin(m) ?? "",
                                        );
                                        setOptimisticReviewStates((states) => ({
                                            ...states,
                                            [m.seq]: "sent",
                                        }));
                                    } finally {
                                        setSendingReplySeq(null);
                                    }
                                }}
                                onCancelTransportReply={async () => {
                                    setOptimisticReviewStates((states) => ({
                                        ...states,
                                        [m.seq]: "cancelled",
                                    }));
                                    await onSetTransportReviewDecision?.(m.seq, "cancelled");
                                }}
                                showCancelTransportReply={
                                    m.review_state !== "sent" &&
                                    m.review_state !== "cancelled" &&
                                    !optimisticReviewStates[m.seq]
                                }
                                showReviewOverride={
                                    !!readChannelOrigin(m) &&
                                    m.kind === "model_turn" &&
                                    (m.review_state === "sent" || m.review_state === "cancelled" ||
                                        !!optimisticReviewStates[m.seq]) &&
                                    !!onSetTransportReviewDecision
                                }
                                onReviewOverride={async () => {
                                    await onSetTransportReviewDecision?.(m.seq, "pending");
                                    setOptimisticReviewStates((states) => {
                                        const next = { ...states };
                                        delete next[m.seq];
                                        return next;
                                    });
                                }}
                                showForceResponse={
                                    !!readChannelOrigin(m) &&
                                    m.kind === "model_turn" &&
                                    isQuietAgentResponse(m.text) &&
                                    !!onForceTransportResponse
                                }
                                onForceResponse={async () => {
                                    await onForceTransportResponse?.(m.seq);
                                }}
                                showRerunResponse={
                                    m.kind === "model_turn" && !!onRerunResponse
                                }
                                rerunResponseBusy={rerunningSeq === m.seq}
                                onRerunResponse={async () => {
                                    if (!onRerunResponse) return;
                                    setRerunningSeq(m.seq);
                                    try {
                                        await onRerunResponse(m.seq);
                                    } finally {
                                        setRerunningSeq(null);
                                    }
                                }}
                            />}
                            </Fragment>
                        );
                    }
                    const Renderer = getCardRenderer(item.card.kind);
                    // 2026-05-04 — wrap every card in `.execlaw-msg`
                    // so it inherits the same centered + clamped
                    // reading-column treatment that messages get via
                    // MessageBubble. Without this wrapper, cards
                    // (research card, attachment chip, etc.) anchored
                    // to the outer scroll surface and ran the full
                    // viewport width — out of alignment with the
                    // surrounding chat bubbles. The renderer's own
                    // outer `<div className="execlaw-card-…">` keeps
                    // its kind-specific styling; the wrapper just
                    // owns column placement.
                    return (
                        <div
                            key={`card-${item.card.card_id}`}
                            className="execlaw-msg execlaw-msg--card"
                        >
                            <Renderer card={item.card} />
                        </div>
                    );
                })}
                {/* Live "what's the agent doing" pulse, mounted
                    inside the message stream where the next
                    assistant message will land. Shows nothing
                    when there's no active tool — only renders
                    while a `tool_call` round-trip is in flight. */}
                <ToolActivityPill conversationId={conversationId} />
                {streaming && (
                    <div className="execlaw-msg" data-testid="streaming-bubble">
                        <div className="execlaw-msg__meta">
                            {nexus ? "execlaw" : "agent"} · streaming
                            <span className="execlaw-streaming-cursor" aria-hidden>
                                ▍
                            </span>
                        </div>
                        <div className={`execlaw-msg__bubble${nexus ? " is-agent" : ""}`}>
                            <MarkdownContent text={streaming} streaming />
                        </div>
                    </div>
                )}
            </div>
            {!isAtBottom && (
                <button
                    type="button"
                    className="execlaw-scroll-to-bottom"
                    onClick={scrollToBottom}
                    aria-label={`Scroll to latest message${Object.keys(buffered).length ? `, ${Object.entries(buffered).map(([source, count]) => `${count} new from ${source}`).join(", ")}` : ""}`}
                    data-testid="scroll-to-bottom"
                >
                    <i className="bi bi-arrow-down" aria-hidden />
                    {nexus && Object.keys(buffered).length > 0 && <span className="execlaw-nexus__arrival-count" aria-live="polite">
                        {Object.entries(buffered).map(([source, count]) => `${source}: ${count}`).join(" · ")}
                    </span>}
                </button>
            )}
        </div>
    );
}

/// Render unix-seconds as a compact local date+time stamp for chat
/// metadata lines. Returns an empty string for invalid inputs.
export function formatMessageTimestamp(unixSeconds: number): string {
    if (!Number.isFinite(unixSeconds) || unixSeconds <= 0) {
        return "";
    }
    const d = new Date(unixSeconds * 1000);
    if (Number.isNaN(d.getTime())) {
        return "";
    }
    return new Intl.DateTimeFormat(undefined, {
        year: "numeric",
        month: "short",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
    }).format(d);
}

function MessageBubble({
    message,
    nexus = false,
    highlighted = false,
    sourceMatch = false,
    dimmed = false,
    annotation,
    messageChoices = [],
    onSaveAnnotation,
    replySource,
    onJumpToMessage,
    showTransportSend = false,
    transportSendBusy = false,
    transportChoices = [],
    selectedTransport = "",
    onTransportChange,
    onSendTransportReply,
    onCancelTransportReply,
    showCancelTransportReply = false,
    showReviewOverride = false,
    onReviewOverride,
    showForceResponse = false,
    onForceResponse,
    showRerunResponse = false,
    rerunResponseBusy = false,
    onRerunResponse,
}: {
    message: MessageView;
    nexus?: boolean;
    highlighted?: boolean;
    sourceMatch?: boolean;
    dimmed?: boolean;
    annotation?: NexusAnnotation;
    messageChoices?: MessageView[];
    onSaveAnnotation?: (annotation: NexusAnnotation) => Promise<void>;
    replySource?: MessageView;
    onJumpToMessage?: (seq: number) => void;
    showTransportSend?: boolean;
    transportSendBusy?: boolean;
    transportChoices?: AvailableTransportView[];
    selectedTransport?: string;
    onTransportChange?: (channel: string) => void;
    onSendTransportReply?: () => Promise<void>;
    onCancelTransportReply?: () => void;
    showCancelTransportReply?: boolean;
    showReviewOverride?: boolean;
    onReviewOverride?: () => Promise<void>;
    showForceResponse?: boolean;
    onForceResponse?: () => Promise<void>;
    showRerunResponse?: boolean;
    rerunResponseBusy?: boolean;
    onRerunResponse?: () => Promise<void>;
}) {
    // 2026-05-15 — read AuthContext directly (not via the `useAuth()`
    // wrapper that throws when there's no provider). MessageStream
    // unit tests render the component in isolation without an
    // `<AuthProvider>`; gracefully degrading to `null` keeps those
    // tests passing — the only consequence is that the access-token
    // query param on `<img>` attachment URLs is absent, which would
    // 401 against the live server but doesn't affect those tests.
    const auth = useContext(AuthContext);
    const t = useT();
    const [branchDraft, setBranchDraft] = useState("");
    const [tagDraft, setTagDraft] = useState("");
    const [linkTarget, setLinkTarget] = useState("");
    const [linkRelation, setLinkRelation] = useState<NexusLink["relation"]>("replies_to");
    const [linkDrafts, setLinkDrafts] = useState<NexusLink[]>([]);
    const [organizeError, setOrganizeError] = useState("");
    useEffect(() => {
        setBranchDraft(annotation?.branch_id ?? "");
        setTagDraft(annotation?.tags.join(", ") ?? "");
        setLinkDrafts(annotation?.links ?? []);
    }, [annotation]);
    const role = roleFor(message);
    // 2026-05-15 — when the operator picked a skill from the
    // composer's `+` menu, the server prepended a `<skill
    // name="...">...</skill>\n\n` block onto the user_msg text
    // before the model saw it. Strip those blocks here so the chat
    // bubble shows only what the operator actually typed; the
    // skills surface as chips below the bubble instead. Stripping
    // is keyed off `applied_skill_names` being non-empty so we
    // never accidentally trim text the user actually typed that
    // happens to start with `<skill ...>`.
    const appliedSkills = message.applied_skill_names ?? [];
    const rawText = message.text ?? renderToolFallback(message);
    const text =
        appliedSkills.length > 0
            ? stripSkillPrependBlock(rawText)
            : rawText;
    const channelOrigin = readChannelOrigin(message);

    // Long messages render in full — see the file-header note for
    // why we dropped the Phase-6 clamp + "Read more" affordance.

    // Rich tool-result rendering: if a tool_result message's payload
    // is JSON carrying a `chat_component_kind` marker and a renderer
    // is registered for that kind, render via the chat-component
    // registry instead of dumping raw JSON. Falls back to plain text
    // for unknown kinds or non-JSON payloads.
    const chatComponent =
        message.kind === "tool_result" ? detectChatComponent(text) : null;
    const ChatComponentRenderer = chatComponent
        ? getChatComponent(chatComponent.kind)
        : undefined;

    // 2026-04-28 — meta-line cleanup:
    //   * Hide the channel-origin icon for the default `web` origin
    //     (adds visual noise; only useful when a non-web transport
    //     delivered the message — Signal / email / voice / sms).
    //   * Skip the `· {actor}` suffix when the actor is redundant
    //     with the role — "agent · agent" was the regression we
    //     caught here. The runner stamps `actor: "agent"` on every
    //     `model_turn` event; collapsing that into just "agent" reads
    //     cleaner.
    //   * Drop the meta entirely for the common case (web-origin
    //     user message). The right-aligned pill already telegraphs
    //     "you said this" — a "you" label adds no info. We DO keep
    //     the meta when a user message arrives via a non-web
    //     transport (Signal / email / voice / sms) so the operator
    //     can see "this came in over Signal" without inspecting the
    //     event payload.
    const showOriginIcon = channelOrigin !== "web";
    const actorSuffix =
        message.actor && message.actor !== role
            ? ` · ${message.actor}`
            : "";
    const isUserMessage =
        message.kind === "user_msg" || message.kind === "cold_contact_arrived";
    const isWhatsAppMessage = channelOrigin === "whatsapp";
    const timestamp = formatMessageTimestamp(message.committed_at);

    let metaText = role + actorSuffix;
    if (isUserMessage && channelOrigin === "web") {
        metaText = "you";
    }
    if (timestamp) {
        metaText = `${metaText} · ${timestamp}`;
    }
    const transportMeta =
        isUserMessage && channelOrigin !== "web" && message.transport_context
            ? timestamp
                ? `${message.transport_context} · ${timestamp}`
                : message.transport_context
            : metaText;

    // 2026-05-15 — image attachments. The server emits these on
    // user_msg events the operator submitted through the composer's
    // `+` menu. Each entry has an `id` and `mime`; the id is either:
    //   * a persisted attachment id (server-minted) → served via
    //     `/api/attachments/<id>` and gated by the auth cookie /
    //     bearer the SPA already holds, OR
    //   * a `data:` URL when the entry is still optimistic (the SPA
    //     just sent it; canonical id arrives once listMessages
    //     refetches). The `<img>` accepts both verbatim.
    const attachments = message.attachments ?? [];
    const replyExcerpt = replySource
        ? (replySource.applied_skill_names?.length
            ? stripSkillPrependBlock(replySource.text ?? "")
            : replySource.text ?? "").slice(0, 160)
        : "";

    return (
        <div
            data-message-seq={message.seq}
            data-source-tone={nexus ? sourceTone(messageSource(message)) : undefined}
            tabIndex={nexus ? -1 : undefined}
            aria-label={nexus ? `${messageSource(message)} #${message.seq}` : undefined}
            className={
                "execlaw-msg" +
                (isUserMessage ? " is-user" : "") +
                (message.reply_to_seq != null ? " is-linked-reply" : "") +
                (nexus && message.transport_group ? " is-group-member" : "") +
                (nexus && highlighted ? " is-source-active" : "") +
                (nexus && sourceMatch ? " is-source-match" : "") +
                (nexus && dimmed ? " is-unrelated" : "") +
                (nexus && annotation?.branch_id ? " is-branched" : "") +
                (nexus && annotation?.links.some((link) => link.relation === "mentions") ? " is-mentioned" : "")
            }
        >
            {nexus && (
                <div className="execlaw-nexus__message-header">
                    <span className="execlaw-nexus__node" aria-hidden>
                        <i className={`bi ${message.kind === "model_turn" ? "bi-stars" : isToolKind(message.kind) ? "bi-braces" : "bi-arrow-down-left"}`} />
                    </span>
                    <strong>{messageSource(message)}</strong>
                    <span className="execlaw-nexus__kind">
                        {message.kind === "model_turn" ? t("chat.agentResponse", "Agent response")
                            : message.kind === "tool_use" ? t("chat.toolRequest", "Tool request")
                            : message.kind === "tool_result" ? t("chat.toolResult", "Tool result")
                            : t("chat.incoming", "Incoming")}
                    </span>
                    <span className="execlaw-nexus__sequence">#{message.seq}</span>
                </div>
            )}
            {nexus && annotation?.branch_id && <span className="execlaw-nexus__branch-label">
                <i className="bi bi-diagram-2" aria-hidden /> Branch: {annotation.branch_id}
            </span>}
            {nexus && !!annotation?.tags.length && <div className="execlaw-nexus__tags" aria-label="Message tags">
                {annotation.tags.map((tag) => <span key={tag}><i className="bi bi-tag" aria-hidden /> {tag}</span>)}
            </div>}
            {nexus && annotation?.links.map((link) => {
                const target = messageChoices.find((candidate) => candidate.seq === link.target_seq);
                return <button key={`${link.relation}:${link.target_seq}`} type="button"
                    className="execlaw-nexus__relation" disabled={!target}
                    onClick={() => onJumpToMessage?.(link.target_seq)}>
                    <i className="bi bi-arrow-return-up" aria-hidden />
                    <span>{link.relation.replaceAll("_", " ")} #{link.target_seq} · {target ? messageSource(target) : "Message not loaded"}</span>
                </button>;
            })}
            {nexus && message.reply_to_seq != null && (
                <button
                    type="button"
                    className="execlaw-nexus__relation"
                    disabled={!replySource}
                    onClick={() => onJumpToMessage?.(message.reply_to_seq!)}
                    title={replySource ? `#${replySource.seq}: ${replyExcerpt}` : undefined}
                >
                    <i className="bi bi-arrow-return-up" aria-hidden />
                    <span>{t("chat.replySource", "Reply to")} #{message.reply_to_seq}
                        {replySource ? ` · ${messageSource(replySource)}` : ` · ${t("chat.sourceNotLoaded", "Source not loaded")}`}
                    </span>
                    {replySource && <span className="execlaw-nexus__excerpt">{replyExcerpt}</span>}
                </button>
            )}
            {!nexus && message.reply_to_seq != null && (
                <div className="execlaw-msg__reply-link">
                    <i className="bi bi-arrow-return-right" aria-hidden />
                    Reply to the incoming {channelOrigin} message
                </div>
            )}
            {nexus && message.kind === "model_turn" && channelOrigin !== "web" && <span className="execlaw-nexus__status">
                <i className="bi bi-circle-half" aria-hidden />
                {message.review_state === "sent" ? "Send requested" : message.review_state === "cancelled" ? "Cancelled" : "Awaiting review"}
            </span>}
            <div className="execlaw-msg__meta">
                {showOriginIcon && (
                    <ChannelOriginIcon origin={channelOrigin} />
                )}
                {transportMeta !== metaText ? (
                    <span className="execlaw-msg__transport-context">{transportMeta}</span>
                ) : transportMeta}
                {message.transport_context &&
                    message.channel_origin &&
                    transportMeta === metaText && (
                    <span className="execlaw-msg__transport-context">
                        {message.transport_context}
                    </span>
                )}
                {message.kind === "model_turn" && message.history_matches != null && (
                    <span className="execlaw-msg__history-badge" title="Related archived transport history used in this response">
                        <i className="bi bi-clock-history" aria-hidden />
                        history considered: {message.history_matches}
                    </span>
                )}
            </div>
            <div
                className={
                    "execlaw-msg__bubble" +
                    (isUserMessage ? " is-user" : "") +
                    (message.kind === "model_turn" ? " is-agent" : "") +
                    (isWhatsAppMessage ? " is-whatsapp" : "") +
                    (isToolKind(message.kind) ? " is-tool" : "") +
                    (ChatComponentRenderer ? " is-rich-component" : "")
                }
            >
                {attachments.length > 0 && (
                    <div
                        className="execlaw-msg__attachments"
                        data-testid="message-attachments"
                    >
                        {attachments.map((a) => (
                            <AttachmentMedia
                                key={a.id}
                                id={a.id}
                                mime={a.mime}
                                filename={a.filename ?? null}
                                sizeBytes={a.size_bytes ?? null}
                                getToken={auth?.getAccessToken}
                            />
                        ))}
                    </div>
                )}
                {/* Tool messages are JSON / monospace dumps by default —
                    render as raw text. Exception: when the payload
                    carries a `chat_component_kind` marker AND a
                    matching renderer is registered (open-meteo's
                    weather / chart components), dispatch to the
                    component instead. Everything else (agent + user)
                    goes through the markdown pipeline. */}
                {text.length > 0 &&
                    (ChatComponentRenderer && chatComponent ? (
                        <ChatComponentRenderer data={chatComponent.data} />
                    ) : isToolKind(message.kind) ? (
                        text
                    ) : (
                        <MarkdownContent text={text} />
                    ))}
                {appliedSkills.length > 0 && (
                    <div
                        className="execlaw-msg__applied-skills"
                        data-testid="message-applied-skills"
                    >
                        <i className="bi bi-stars" aria-hidden />
                        <span className="execlaw-msg__applied-skills-label">
                            applied:
                        </span>
                        {appliedSkills.map((name, i) => (
                            <span
                                key={name}
                                className="execlaw-msg__applied-skill-chip"
                                data-testid="message-applied-skill"
                                data-skill-name={name}
                            >
                                {name}
                                {i < appliedSkills.length - 1 ? "," : ""}
                            </span>
                        ))}
                    </div>
                )}
                {showTransportSend && (
                    <div className="d-flex gap-2 mt-3">
                        <select
                            className="form-select form-select-sm"
                            value={selectedTransport}
                            onChange={(event) => onTransportChange?.(event.target.value)}
                            aria-label="Send reply via transport"
                            data-testid="send-transport-select"
                        >
                            {transportChoices.length === 0 && (
                                <option value={selectedTransport}>
                                    {transportLabel(selectedTransport)}
                                </option>
                            )}
                            {transportChoices.map((transport) => (
                                <option key={transport.id} value={transport.id}>
                                    {transport.label}
                                </option>
                            ))}
                        </select>
                        <button
                            type="button"
                            className="btn btn-sm btn-outline-success"
                            disabled={transportSendBusy}
                            onClick={() => void onSendTransportReply?.()}
                            data-testid="send-transport-reply"
                        >
                            <i className="bi bi-send me-1" aria-hidden />
                            {transportSendBusy
                                ? "Sending..."
                                : message.review_state === "sent" || message.review_state === "cancelled"
                                  ? `Resend to ${transportLabel(message.channel_origin)}`
                                  : `Send to ${transportLabel(message.channel_origin)}`}
                        </button>
                        {showCancelTransportReply && (
                        <button
                            type="button"
                            className="btn btn-sm btn-outline-secondary"
                            disabled={transportSendBusy}
                            onClick={onCancelTransportReply}
                            data-testid="cancel-transport-reply"
                        >
                            <i className="bi bi-x-lg me-1" aria-hidden />
                            Cancel reply
                        </button>
                        )}
                    </div>
                )}
                {showReviewOverride && (
                    <button
                        type="button"
                        className="btn btn-sm btn-outline-secondary mt-3"
                        onClick={() => void onReviewOverride?.()}
                        data-testid="override-transport-review"
                    >
                        <i className="bi bi-arrow-counterclockwise me-1" aria-hidden />
                        Override decision
                    </button>
                )}
                {showForceResponse && (
                    <button
                        type="button"
                        className="btn btn-sm btn-outline-info mt-3"
                        onClick={() => void onForceResponse?.()}
                        data-testid="force-transport-response"
                    >
                        <i className="bi bi-arrow-repeat me-1" aria-hidden />
                        Force response
                    </button>
                )}
                {showRerunResponse && (
                    <button
                        type="button"
                        className="btn btn-sm btn-outline-info mt-3"
                        disabled={rerunResponseBusy}
                        onClick={() => void onRerunResponse?.()}
                        data-testid="rerun-response"
                    >
                        <i className="bi bi-arrow-clockwise me-1" aria-hidden />
                        {rerunResponseBusy ? "Rerunning..." : "Rerun response"}
                    </button>
                )}
            </div>
            {nexus && onSaveAnnotation && <details className="execlaw-nexus__organize">
                <summary><i className="bi bi-diagram-3" aria-hidden /> Organize message #{message.seq}</summary>
                <div className="execlaw-nexus__organize-fields">
                    <label>Branch
                        <input value={branchDraft} maxLength={120} placeholder="Branch name"
                            onChange={(event) => setBranchDraft(event.target.value)} />
                    </label>
                    <label>Tags
                        <input value={tagDraft} placeholder="Comma-separated tags"
                            onChange={(event) => setTagDraft(event.target.value)} />
                    </label>
                    <label>Related message
                        <select value={linkTarget} onChange={(event) => setLinkTarget(event.target.value)}>
                            <option value="">Select message</option>
                            {messageChoices.filter((candidate) => candidate.seq !== message.seq).map((candidate) =>
                                <option key={candidate.seq} value={candidate.seq}>#{candidate.seq} · {messageSource(candidate)}</option>)}
                        </select>
                    </label>
                    <label>Relationship
                        <select value={linkRelation} onChange={(event) => setLinkRelation(event.target.value as NexusLink["relation"])}>
                            <option value="replies_to">Replies to</option>
                            <option value="forwarded_from">Forwarded from</option>
                            <option value="mentions">Mentions</option>
                            <option value="generated_from">Generated from</option>
                        </select>
                    </label>
                    <button type="button" disabled={!linkTarget} title="Add relationship" aria-label="Add relationship"
                        onClick={() => { setLinkDrafts((links) => [...links.filter((link) => !(link.target_seq === Number(linkTarget) && link.relation === linkRelation)), { target_seq: Number(linkTarget), relation: linkRelation }]); setLinkTarget(""); }}>
                        <i className="bi bi-plus-lg" aria-hidden />
                    </button>
                </div>
                {linkDrafts.map((link) => <div key={`${link.target_seq}:${link.relation}`}>
                    {link.relation.replaceAll("_", " ")} #{link.target_seq}
                    <button type="button" title={`Remove relationship to #${link.target_seq}`} aria-label={`Remove relationship to #${link.target_seq}`}
                        onClick={() => setLinkDrafts((links) => links.filter((item) => item !== link))}>
                        <i className="bi bi-x" aria-hidden />
                    </button>
                </div>)}
                <button type="button" className="execlaw-nexus__organize-save"
                    onClick={() => {
                        const tags = [...new Set(tagDraft.split(",").map((tag) => tag.trim()).filter(Boolean))];
                        void onSaveAnnotation({ seq: message.seq, branch_id: branchDraft.trim() || null, tags, links: linkDrafts })
                            .then(() => setOrganizeError(""))
                            .catch(() => setOrganizeError("Unable to save organization"));
                    }}>Save organization</button>
                {organizeError && <span role="alert">{organizeError}</span>}
            </details>}
        </div>
    );
}

function isQuietAgentResponse(text: string | null): boolean {
    const normalized = (text ?? "").trim();
    if (
        normalized === "" ||
        /^\[?empty response\]?$/i.test(normalized) ||
        /^\(empty response\)$/i.test(normalized)
    ) {
        return true;
    }
    return /\b(stay quiet|not directed at me|no response needed|won't respond|will not respond)\b/i.test(
        normalized,
    );
}

function transportLabel(channel: MessageView["channel_origin"]): string {
    if (channel === "signal") return "Signal";
    if (channel === "whatsapp") return "WhatsApp";
    if (channel === "sms") return "SMS";
    if (channel === "email") return "email";
    return "transport";
}

/// Strip the leading `<skill name="...">...</skill>` blocks the
/// server prepends onto a user_msg when the operator picked skills
/// from the composer's `+` menu. The block format is fixed:
///
/// ```text
/// <skill name="ns/short">
/// {body}
/// </skill>
///
/// {original user text}
/// ```
///
/// We match one OR MORE leading blocks (multi-skill picks) followed
/// by any blank lines, then return whatever's left. Caller only
/// invokes this when `applied_skill_names` is non-empty so we never
/// trim text the user typed that happens to look like a skill
/// block.
export function stripSkillPrependBlock(text: string): string {
    // Greedy match for any number of consecutive `<skill name="X">
    // ... </skill>` blocks at the very start of the text, plus any
    // trailing whitespace before the user's actual content. The
    // `[\s\S]*?` (non-greedy) inside each block stops at the first
    // `</skill>` so we don't accidentally swallow user text that
    // contains the literal substring.
    const stripped = text.replace(
        /^(<skill name="[^"]*">[\s\S]*?<\/skill>\s*)+/,
        "",
    );
    return stripped;
}

function readChannelOrigin(m: MessageView): ChannelOrigin {
    // The server attaches a channel_origin field to user_msg +
    // model_turn events that flowed through a transport bridge
    // (signal / email / voice / sms). Web-originated turns leave it
    // absent; the SPA defaults to "web" and shows no icon.
    const raw = (m as MessageView & { channel_origin?: unknown }).channel_origin;
    return typeof raw === "string" && raw.trim() ? raw.trim() : "web";
}

type ChannelOrigin = string;

function messageSource(message: MessageView): string {
    if (message.kind === "model_turn") return "execlaw";
    if (isToolKind(message.kind)) return message.actor || "Tools";
    const channel = readChannelOrigin(message);
    const labels: Record<string, string> = {
        web: "Web", signal: "Signal", whatsapp: "WhatsApp", email: "Email",
        sms: "SMS", voice: "Voice", slack: "Slack", discord: "Discord",
    };
    return Object.hasOwn(labels, channel) ? labels[channel] : channel;
}

function sourceTone(source: string): number {
    const known: Record<string, number> = { execlaw: 0, WhatsApp: 1, Signal: 2, Email: 3, Web: 4, Tools: 5 };
    if (Object.hasOwn(known, source)) return known[source];
    let hash = 0;
    for (const character of source) hash = (hash * 31 + character.charCodeAt(0)) >>> 0;
    return hash % 6;
}

function ChannelOriginIcon({ origin }: { origin: ChannelOrigin }) {
    return (
        <ChannelIcon
            channel={origin}
            size="1em"
            decorative={false}
            className="execlaw-channel-origin me-2"
            data-testid="channel-origin"
        />
    );
}

function roleFor(m: MessageView): string {
    switch (m.kind) {
        case "user_msg":
        case "cold_contact_arrived":
            return "you";
        case "model_turn":
            return "agent";
        case "tool_use":
            return "tool · request";
        case "tool_result":
            return "tool · result";
        default:
            return m.kind;
    }
}

function isToolKind(kind: string): boolean {
    return kind === "tool_use" || kind === "tool_result";
}

function renderToolFallback(m: MessageView): string {
    return `[${m.kind} (no text payload)]`;
}

/// One attachment under a message bubble. Handles both the
/// `<img>` (image MIME) and `<a download>` (file chip) layouts.
///
/// 2026-05-19 — the URL is fetched from `POST /api/downloads/sign`
/// on mount (replacing the pre-fix `?access_token=<jwt>` query
/// param that the security audit flagged as broad leak surface).
/// During the brief async window before the signed URL resolves
/// the `<img>` shows alt text / the chip is disabled. Optimistic
/// data URLs (in-flight upload) bypass the sign call.
function AttachmentMedia({
    id,
    mime,
    filename,
    sizeBytes,
    getToken,
}: {
    id: string;
    mime: string;
    filename: string | null;
    sizeBytes: number | null;
    getToken: (() => string | null) | undefined;
}) {
    const isDataUrl = id.startsWith("data:");
    const basePath = isDataUrl
        ? null
        : `/api/attachments/${encodeURIComponent(id)}`;
    const [signedUrl, setSignedUrl] = useState<string | null>(null);
    useEffect(() => {
        if (!basePath || !getToken) {
            setSignedUrl(null);
            return;
        }
        let cancelled = false;
        signDownloadUrl(basePath, getToken)
            .then((u) => {
                if (!cancelled) setSignedUrl(u);
            })
            .catch(() => {
                if (!cancelled) setSignedUrl(null);
            });
        return () => {
            cancelled = true;
        };
    }, [basePath, getToken]);
    const src = isDataUrl ? id : signedUrl;
    if (isImageMime(mime)) {
        if (!src) {
            // No render until the signed URL lands — keeps the
            // browser from issuing a 401 GET it would just retry.
            return null;
        }
        return (
            <img
                src={src}
                alt="attached image"
                className="execlaw-msg__attachment-image"
            />
        );
    }
    // File-chip path. Without a signed URL we still render the
    // chip but without an `href`, so the affordance is visible
    // (filename + icon) even while the URL is being signed.
    return (
        <a
            href={src ?? undefined}
            download={filename ?? undefined}
            className="execlaw-msg__attachment-file"
            data-testid="message-attachment-file"
            data-mime={mime}
            title={`${filename ?? "file"} · ${mime}`}
            aria-disabled={src ? undefined : true}
        >
            <i className={fileIconForMime(mime)} aria-hidden />
            <span className="execlaw-msg__attachment-file-name">
                {filename ?? "attachment"}
            </span>
            {typeof sizeBytes === "number" && sizeBytes > 0 && (
                <span className="execlaw-msg__attachment-file-size">
                    {formatBytes(sizeBytes)}
                </span>
            )}
        </a>
    );
}
