// randomUuid must work on plain-HTTP LAN deployments where the browser
// withholds crypto.randomUUID (non-secure context).

import { afterEach, describe, expect, it, vi } from "vitest";
import { randomUuid } from "../api/randomId";

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

describe("randomUuid", () => {
    afterEach(() => {
        vi.unstubAllGlobals();
    });

    it("uses crypto.randomUUID when available", () => {
        const fixed = "11111111-2222-4333-8444-555555555555";
        vi.stubGlobal("crypto", {
            randomUUID: () => fixed,
            getRandomValues: (a: Uint8Array) => a,
        });
        expect(randomUuid()).toBe(fixed);
    });

    it("falls back to getRandomValues in a non-secure context", () => {
        const getRandomValues = vi.fn((a: Uint8Array) => {
            a.fill(0xff);
            return a;
        });
        vi.stubGlobal("crypto", { getRandomValues });
        const id = randomUuid();
        expect(getRandomValues).toHaveBeenCalledOnce();
        expect(id).toMatch(UUID_V4);
        expect(id).toBe("ffffffff-ffff-4fff-bfff-ffffffffffff");
    });

    it("produces distinct v4 ids without randomUUID", () => {
        vi.stubGlobal("crypto", {
            getRandomValues: (a: Uint8Array) => {
                for (let i = 0; i < a.length; i++) a[i] = Math.floor(Math.random() * 256);
                return a;
            },
        });
        const ids = new Set(Array.from({ length: 50 }, () => randomUuid()));
        expect(ids.size).toBe(50);
        for (const id of ids) expect(id).toMatch(UUID_V4);
    });
});
