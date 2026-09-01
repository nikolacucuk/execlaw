"""Fix ConversationRow initializers that were broken by the bulk regex.

The regex incorrectly turned:
    last_activity_at: 0,
            })
into:
    last_activity_at: 0,
    context_window_policy: None,
            )

This script finds all *.rs files and:
1. Fixes the broken pattern (context_window_policy line followed by bare ')') 
   back to:
       context_window_policy: None,
               })
2. Also adds context_window_policy: None after last_activity_at where missing.
"""
import re
import sys
from pathlib import Path

root = Path("crates")
rs_files = list(root.rglob("*.rs"))

fixed_files = []

for fpath in rs_files:
    text = fpath.read_text(encoding="utf-8")
    original = text

    # Fix broken pattern: context_window_policy: None,\n<indent>)
    # The `)` alone at line means it was `})` before regex ran.
    def fix_broken(m):
        indent_cwp = m.group(1)   # indentation of context_window_policy line
        indent_close = m.group(2)  # indentation of the lone )
        return f"{indent_cwp}context_window_policy: None,\n{indent_close}}}"

    text = re.sub(
        r'^([ \t]+)context_window_policy: None,\n([ \t]+)\)$',
        fix_broken,
        text,
        flags=re.MULTILINE,
    )

    if text != original:
        fpath.write_text(text, encoding="utf-8")
        fixed_files.append(str(fpath))

print(f"Fixed {len(fixed_files)} files:")
for f in fixed_files:
    print(f"  {f}")
