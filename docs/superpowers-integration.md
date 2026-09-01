# Superpowers -> execlaw Integration Playbook

This document explains how to salvage and integrate Superpowers skills into this execlaw workspace with a clean split between shared and user-specific content.

## Goal

- Keep community/general skills shared and reusable.
- Keep operator-specific behavior (DjEnKa preferences, private workflow variants) user-scoped.
- Reuse execlaw's existing skill subsystem instead of inventing a second mechanism.

## What execlaw already gives you

- Skill storage in SQLite (`state_skills`, `state_skill_versions`, proposals).
- Skill-level lifecycle (`trial` -> `stable` -> `archived`).
- Plugin-shipped skill import with namespacing (`<plugin_id>/<skill>`).
- Admin API and UI for listing, creating, editing, promoting, archiving skills.

This means Superpowers should be integrated as skill content and workflow conventions, not as a parallel runtime.

## Scope model (shared vs user)

Use two plugin namespaces:

1. Shared namespace
- Plugin id: `superpowers-shared`
- Content: team/community reusable skills from Superpowers.
- Ownership: safe to sync/update from upstream.

2. User namespace (DjEnKa)
- Plugin id: `superpowers-user`
- Content: private overlays, personal conventions, environment-specific commands.
- Ownership: only for this operator profile.

Why plugin namespaces:
- execlaw already archives skill rows by `owning_plugin_id` on uninstall.
- Source attribution stays explicit in admin UI (`source`, `registration_kind`, `owning_plugin_id`).
- You can update shared and user layers independently.

## Files added in this repo

- [scripts/build-superpowers-skill-plugins.ps1](scripts/build-superpowers-skill-plugins.ps1)
- [scripts/build-superpowers-skill-plugins.sh](scripts/build-superpowers-skill-plugins.sh)

These scripts generate installable plugin ZIPs from Superpowers skill directories.

## Expected source layout

Shared skills root (default):
- `$HOME/.config/superpowers/skills`

User skills root (default):
- `$HOME/.execlaw/skills/user`

Each skill is discovered from `SKILL.md` files recursively.

## Build and install

### Windows (PowerShell)

```powershell
pwsh scripts/build-superpowers-skill-plugins.ps1
```

Optional overrides:

```powershell
pwsh scripts/build-superpowers-skill-plugins.ps1 `
  -SharedSkillsRoot "$HOME/.config/superpowers/skills" `
  -UserSkillsRoot "$HOME/.execlaw/skills/user" `
  -UserSkillNamespace "djenka"
```

### macOS/Linux

```bash
bash scripts/build-superpowers-skill-plugins.sh
```

Environment overrides:

```bash
SHARED_SKILLS_ROOT="$HOME/.config/superpowers/skills" \
USER_SKILLS_ROOT="$HOME/.execlaw/skills/user" \
USER_SKILL_NAMESPACE="djenka" \
bash scripts/build-superpowers-skill-plugins.sh
```

Output ZIPs are written under `dist/` and can be installed in execlaw:
- Settings -> Plugins -> Install

## Operating model after install

1. Shared updates
- Pull/update your Superpowers checkout.
- Rebuild shared plugin zip.
- Reinstall zip in execlaw.

2. User updates
- Edit/add files under your user skills root.
- Rebuild user plugin zip.
- Reinstall user zip.

3. Promotion flow
- Imported skills land as `trial`.
- Promote selectively to `stable` in Skills admin once validated.

## Recommended curation for first import

Start with high-signal, low-risk skills:
- writing-plans
- requesting-code-review
- systematic-debugging
- test-driven-development
- executing-plans

Then expand incrementally based on observed value.

## Notes on boundaries

- Do not put secrets in shared skill files.
- Put machine-specific paths, private repos, and personal identifiers in user-scoped skills only.
- Keep user overlays small and composable to reduce drift from shared upstream.

## Future hardening opportunities

- Add a small validator that rejects oversized skill bundles before packaging.
- Add an allowlist/denylist file so only selected Superpowers skills are exported.
- Add CI task that rebuilds the shared zip and checks manifest validity.
