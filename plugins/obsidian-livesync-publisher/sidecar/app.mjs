import { createServer } from "node:http";
import { readFile, readdir, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, relative, sep } from "node:path";
import { DirectFileManipulator } from "@vrtmrz/livesync-commonlib";

const VAULT_ROOT = "/ai_pool";
const PORT = Number.parseInt(process.env.PORT ?? "8080", 10);
const MAX_REQUEST_BYTES = 256 * 1024;

function respond(response, status, body) {
    const encoded = JSON.stringify(body);
    response.writeHead(status, { "content-type": "application/json", "content-length": Buffer.byteLength(encoded) });
    response.end(encoded);
}

function sourceRoot(sourceSubdir, vaultRoot = VAULT_ROOT) {
    if (typeof sourceSubdir !== "string" || !sourceSubdir || sourceSubdir.startsWith("/") || sourceSubdir.includes("\\")) {
        throw new Error("source_subdir must be a relative path below /ai_pool");
    }
    const root = resolve(vaultRoot, sourceSubdir);
    if (relative(vaultRoot, root).startsWith(`..${sep}`) || root === vaultRoot) {
        throw new Error("source_subdir escapes /ai_pool");
    }
    return root;
}

async function sourceFiles(sourceSubdir, maxFiles, maxBytes, vaultRoot = VAULT_ROOT) {
    const root = sourceRoot(sourceSubdir, vaultRoot);
    const pending = [root];
    const files = [];
    let total = 0;
    while (pending.length) {
        const directory = pending.pop();
        let entries;
        try {
            entries = await readdir(directory, { withFileTypes: true });
        } catch (error) {
            if (error.code === "ENOENT") throw new Error(`publisher source directory is missing: ${root}`);
            throw error;
        }
        for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
            if (entry.name.startsWith(".")) continue;
            const fullPath = resolve(directory, entry.name);
            if (entry.isDirectory()) pending.push(fullPath);
            if (!entry.isFile() || !entry.name.endsWith(".md")) continue;
            const fileStat = await stat(fullPath);
            total += fileStat.size;
            if (files.length >= maxFiles) throw new Error(`publisher file limit exceeded (${maxFiles})`);
            if (total > maxBytes) throw new Error(`publisher byte limit exceeded (${maxBytes})`);
            files.push({ logicalPath: `${sourceSubdir}/${relative(root, fullPath).split(sep).join("/")}`, fullPath, fileStat });
        }
    }
    return files.sort((left, right) => left.logicalPath.localeCompare(right.logicalPath));
}

async function withManipulator(payload, action) {
    const passphrase = String(payload.livesync_passphrase ?? "");
    const manipulator = new DirectFileManipulator({
        url: String(payload.couchdb_url ?? "").replace(/\/+$/, ""),
        database: String(payload.database ?? ""),
        username: String(payload.username ?? ""),
        password: String(payload.password ?? ""),
        passphrase: passphrase || undefined,
        obfuscatePassphrase: passphrase ? String(payload.path_obfuscation_passphrase || passphrase) : undefined,
        hashAlg: "xxhash64",
        chunkSplitterVersion: "v3-rabin-karp",
        E2EEAlgorithm: "v2",
    });
    try {
        await manipulator.ready.promise;
        return await action(manipulator);
    } finally {
        await manipulator.close();
    }
}

async function publish(payload) {
    for (const key of ["couchdb_url", "database", "username", "password"]) {
        if (!String(payload[key] ?? "").trim()) throw new Error(`${key} is required`);
    }
    const sourceSubdir = String(payload.source_subdir ?? "").trim();
    const maxFiles = Math.max(1, Math.min(Number(payload.max_files) || 1000, 10000));
    const maxBytes = Math.max(1, Math.min(Number(payload.max_bytes) || 52428800, 52428800));
    const files = await sourceFiles(sourceSubdir, maxFiles, maxBytes);
    let metadataWritten = 0;
    let unchanged = 0;
    await withManipulator(payload, async (manipulator) => {
        for (const file of files) {
            const data = await readFile(file.fullPath, "utf8");
            const existing = await manipulator.get(file.logicalPath);
            const existingData = existing && Array.isArray(existing.data) ? existing.data.join("") : existing && typeof existing.data === "string" ? existing.data : null;
            if (existingData === data) {
                unchanged += 1;
                continue;
            }
            const written = await manipulator.put(file.logicalPath, [data], {
                ctime: file.fileStat.birthtimeMs,
                mtime: file.fileStat.mtimeMs,
                size: file.fileStat.size,
            });
            if (!written) throw new Error(`LiveSync rejected ${file.logicalPath}`);
            metadataWritten += 1;
        }
    });
    return { ok: true, files_seen: files.length, metadata_written: metadataWritten, files_unchanged: unchanged, chunks_written: null, deletions: 0, encrypted: Boolean(payload.livesync_passphrase) };
}

const server = createServer(async (request, response) => {
    if (request.method === "GET" && request.url === "/healthz") return respond(response, 200, { ok: true, source: VAULT_ROOT });
    if (request.method !== "POST" || !["/v1/check", "/v1/publish"].includes(request.url)) return respond(response, 404, { error: "not found" });
    let raw = "";
    request.on("data", (chunk) => {
        raw += chunk;
        if (Buffer.byteLength(raw) > MAX_REQUEST_BYTES) request.destroy();
    });
    request.on("end", async () => {
        try {
            const payload = JSON.parse(raw);
            if (request.url === "/v1/check") {
                const files = await sourceFiles(String(payload.source_subdir ?? "").trim(), 10000, 52428800);
                return respond(response, 200, { ok: true, source: sourceRoot(payload.source_subdir), markdown_files: files.length });
            }
            return respond(response, 200, await publish(payload));
        } catch (error) {
            return respond(response, 400, { ok: false, error: error instanceof Error ? error.message : "publisher failed" });
        }
    });
});

if (process.argv[1] === fileURLToPath(import.meta.url)) {
    server.listen(PORT, "0.0.0.0");
}

export { sourceFiles, sourceRoot };