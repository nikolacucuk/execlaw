# AI Makefile Targets

This folder contains automation targets for setting up agent-specific integration files.

## Current target

### copilot

Run from repo root:

```sh
make -f ai/Makefile copilot
```

Or run from this folder:

```sh
make copilot
```

What it does:

1. Ensures these folders exist at repo root:
   - `.github/`
   - `.archive/`
2. For each destination below, if it already exists, it is moved to `.archive/<name>.<timestamp>`:
   - `.github/agents`
   - `.github/instructions`
   - `.github/skills`
3. Creates symlinks:
   - `.github/agents` -> `ai/agents`
   - `.github/instructions` -> `ai/instructions`
   - `.github/skills` -> `ai/skills`
4. Generates:
   - `.github/copilot-instructions.md`
   - Source: `ai/instructions.md`
   - Path references rewritten from `ai/...` to `.github/...`

## Notes

- The target is safe to re-run. Existing `.github/*` mapping folders are archived before links are recreated.
- On Windows, symlink creation may require Developer Mode or elevated permissions.
- Future targets (for example `claude`, `cursor`) can follow this same pattern in `ai/Makefile`.
