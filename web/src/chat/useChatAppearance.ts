import { useEffect, useState } from "react";

export type ChatAppearance = "classic" | "nexus";
const STORAGE_KEY = "execlaw.chat.appearance";
const CHANGE_EVENT = "execlaw-chat-appearance";

function readAppearance(): ChatAppearance {
    try {
        return localStorage.getItem(STORAGE_KEY) === "nexus" ? "nexus" : "classic";
    } catch {
        return "classic";
    }
}

/** Browser-local chat presentation, synchronized between mounted views and tabs. */
export function useChatAppearance(): [ChatAppearance, (next: ChatAppearance) => void] {
    const [appearance, setAppearance] = useState<ChatAppearance>(readAppearance);

    useEffect(() => {
        const onStorage = (event: StorageEvent) => {
            if (event.key === STORAGE_KEY || event.key === null) setAppearance(readAppearance());
        };
        const onChange = (event: Event) => {
            setAppearance((event as CustomEvent<ChatAppearance>).detail);
        };
        window.addEventListener("storage", onStorage);
        window.addEventListener(CHANGE_EVENT, onChange);
        return () => {
            window.removeEventListener("storage", onStorage);
            window.removeEventListener(CHANGE_EVENT, onChange);
        };
    }, []);

    return [appearance, (next) => {
        setAppearance(next);
        try {
            localStorage.setItem(STORAGE_KEY, next);
        } catch {
            // Restricted storage must not prevent changing the current view.
        }
        window.dispatchEvent(new CustomEvent(CHANGE_EVENT, { detail: next }));
    }];
}