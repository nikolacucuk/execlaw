.PHONY: copilot

REPO_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)

copilot:
	@set -eu; \
	ROOT="$(REPO_ROOT)"; \
	GITHUB_DIR="$$ROOT/.github"; \
	ARCHIVE_DIR="$$ROOT/.archive"; \
	SRC_AGENTS="../ai/agents"; \
	SRC_INSTRUCTIONS="../ai/instructions"; \
	SRC_SKILLS="../ai/skills"; \
	OUT_INSTRUCTIONS="$$GITHUB_DIR/copilot-instructions.md"; \
	mkdir -p "$$GITHUB_DIR" "$$ARCHIVE_DIR"; \
	archive_and_replace() { \
		name="$$1"; \
		src="$$2"; \
		dst="$$GITHUB_DIR/$$name"; \
		if [ -e "$$dst" ] || [ -L "$$dst" ]; then \
			ts="$$(date +%Y%m%d_%H%M%S 2>/dev/null || printf "%s" "$$(powershell -NoProfile -Command "Get-Date -Format yyyyMMdd_HHmmss")")"; \
			archive_target="$$ARCHIVE_DIR/$$name.$$ts"; \
			mv "$$dst" "$$archive_target"; \
			printf 'Archived %s -> %s\n' "$$dst" "$$archive_target"; \
		fi; \
		ln -s "$$src" "$$dst" 2>/dev/null || { \
			if command -v powershell >/dev/null 2>&1; then \
				powershell -NoProfile -Command "New-Item -ItemType SymbolicLink -Path '$$dst' -Target '$$src' -Force | Out-Null"; \
			else \
				printf 'Failed to create symlink: %s -> %s\n' "$$dst" "$$src" >&2; \
				exit 1; \
			fi; \
		}; \
		printf 'Linked %s -> %s\n' "$$dst" "$$src"; \
	}; \
	archive_and_replace agents "$$SRC_AGENTS"; \
	archive_and_replace instructions "$$SRC_INSTRUCTIONS"; \
	archive_and_replace skills "$$SRC_SKILLS"; \
	sed \
		-e 's#ai/agents#.github/agents#g' \
		-e 's#ai/instructions#.github/instructions#g' \
		-e 's#ai/skills#.github/skills#g' \
		"$$ROOT/ai/instructions.md" > "$$OUT_INSTRUCTIONS"; \
	printf 'Wrote %s\n' "$$OUT_INSTRUCTIONS"
