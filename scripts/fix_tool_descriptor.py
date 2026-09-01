#!/usr/bin/env python3
"""Add `sensitive: false,` to all ToolDescriptor { } struct literals that are missing it.

Pattern: find blocks that end with `default_allowed_classes: ...` as their
last field before the closing `},` and inject `sensitive: false,` between them.
"""
import re
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent

# Regex: matches the `default_allowed_classes: <anything>,` line that is
# followed (possibly with whitespace) by the struct-closing `},` — but only
# when there's no `sensitive:` line already between them.
PATTERN = re.compile(
    r'(default_allowed_classes: [^\n]+,)(?!\s*\n\s*sensitive:)(\s*\n(\s*)(?=\}))',
    re.MULTILINE,
)

def fix(path: pathlib.Path) -> int:
    text = path.read_text(encoding='utf-8')
    def replacer(m):
        indent = m.group(3)  # leading whitespace of the closing brace line
        return f'{m.group(1)}\n{indent}    sensitive: false,{m.group(2)}'
    new_text, n = PATTERN.subn(replacer, text)
    if n:
        path.write_text(new_text, encoding='utf-8')
    return n

total_files = 0
total_hits = 0
for f in ROOT.rglob('*.rs'):
    # Skip target directory
    if 'target' in f.parts:
        continue
    try:
        hits = fix(f)
        if hits:
            total_files += 1
            total_hits += hits
            print(f'  {f.relative_to(ROOT)}  (+{hits})')
    except Exception as e:
        print(f'ERROR: {f}: {e}', file=sys.stderr)

print(f'\nDone. Modified {total_files} files, {total_hits} insertions.')
