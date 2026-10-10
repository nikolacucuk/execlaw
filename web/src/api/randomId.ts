/// Returns an RFC 4122 version 4 UUID.
///
/// `crypto.randomUUID` is only exposed in secure contexts (HTTPS,
/// `localhost`, `127.0.0.1`). A LAN deployment served over plain HTTP
/// (e.g. `http://192.168.1.76:3031`) has no `randomUUID`, so calling it
/// throws before the request is sent. `crypto.getRandomValues` is
/// available in every context, so build the UUID from it instead.
///
/// @example
/// const key = randomUuid(); // "3f0c...-4...-8...-..."
export function randomUuid(): string {
    const c: Crypto | undefined =
        typeof crypto !== "undefined" ? crypto : undefined;
    if (c && typeof c.randomUUID === "function") {
        return c.randomUUID();
    }
    const bytes = new Uint8Array(16);
    if (c && typeof c.getRandomValues === "function") {
        c.getRandomValues(bytes);
    } else {
        for (let i = 0; i < bytes.length; i++) {
            bytes[i] = Math.floor(Math.random() * 256);
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0"));
    return [
        hex.slice(0, 4).join(""),
        hex.slice(4, 6).join(""),
        hex.slice(6, 8).join(""),
        hex.slice(8, 10).join(""),
        hex.slice(10, 16).join(""),
    ].join("-");
}
