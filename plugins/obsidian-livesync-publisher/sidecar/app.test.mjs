import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { sourceFiles, sourceRoot } from "./app.mjs";

test("sourceRoot rejects an absolute or escaping source path", () => {
    assert.throws(() => sourceRoot("/etc"));
    assert.throws(() => sourceRoot("../outside"));
    assert.throws(() => sourceRoot("notes\\outside"));
});

test("sourceFiles lists Markdown recursively and skips hidden paths", async () => {
    const root = await mkdtemp(join(tmpdir(), "publisher-source-"));
    const source = "notes";
    const mounted = join(root, source);
    await mkdir(join(mounted, "nested"), { recursive: true });
    await mkdir(join(mounted, ".hidden"), { recursive: true });
    await writeFile(join(mounted, "one.md"), "# one\n");
    await writeFile(join(mounted, "nested", "two.md"), "# two\n");
    await writeFile(join(mounted, ".hidden", "ignored.md"), "# ignored\n");

    const files = await sourceFiles(source, 10, 1024, root);
    assert.deepEqual(files.map((file) => file.logicalPath), [
        `${source}/nested/two.md`,
        `${source}/one.md`,
    ]);
});