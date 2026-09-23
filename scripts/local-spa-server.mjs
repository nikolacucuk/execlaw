import http from "node:http";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";

const root = path.resolve("web/dist");
const contentTypes = {
    ".css": "text/css",
    ".html": "text/html",
    ".js": "text/javascript",
    ".json": "application/json",
    ".jpg": "image/jpeg",
    ".png": "image/png",
    ".svg": "image/svg+xml",
    ".woff2": "font/woff2",
};

function readRequestBody(request) {
    return new Promise((resolve, reject) => {
        const chunks = [];
        request.on("data", (chunk) => chunks.push(chunk));
        request.on("end", () => resolve(Buffer.concat(chunks)));
        request.on("error", reject);
    });
}

const server = http.createServer(async (request, response) => {
    if (request.url.startsWith("/api/")) {
        try {
            const body = ["GET", "HEAD"].includes(request.method)
                ? undefined
                : await readRequestBody(request);
            const backendResponse = await fetch(
                `http://127.0.0.1:3031${request.url}`,
                {
                    body,
                    headers: request.headers,
                    method: request.method,
                },
            );
            response.writeHead(
                backendResponse.status,
                Object.fromEntries(backendResponse.headers),
            );
            response.end(Buffer.from(await backendResponse.arrayBuffer()));
        } catch {
            response.writeHead(502, { "Content-Type": "text/plain" });
            response.end("Backend unavailable");
        }
        return;
    }

    let filePath = path.join(root, decodeURIComponent(request.url.split("?")[0]));
    if (request.url === "/" || !fs.existsSync(filePath) || fs.statSync(filePath).isDirectory()) {
        filePath = path.join(root, "index.html");
    }
    response.writeHead(200, {
        "Cache-Control": "no-cache",
        "Content-Type": contentTypes[path.extname(filePath)] ?? "application/octet-stream",
    });
    fs.createReadStream(filePath).pipe(response);
});

server.on("upgrade", (request, socket, head) => {
    const upstream = net.connect(3031, "127.0.0.1", () => {
        const headers = [
            `${request.method} ${request.url} HTTP/${request.httpVersion}`,
            ...Object.entries(request.headers).map(([name, value]) => {
                const headerValue = Array.isArray(value) ? value.join(", ") : value;
                return `${name}: ${headerValue}`;
            }),
            "",
            "",
        ].join("\r\n");
        upstream.write(headers);
        if (head.length > 0) upstream.write(head);
        socket.pipe(upstream).pipe(socket);
    });
    upstream.on("error", () => socket.destroy());
    socket.on("error", () => upstream.destroy());
});

server.listen(5174, "127.0.0.1", () => {
    console.log("execlaw SPA http://127.0.0.1:5174/");
});