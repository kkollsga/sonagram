.PHONY: check-free-space prune-target check-dev-docs

check-free-space:
	@./scripts/check_free_space.sh

prune-target:
	@./scripts/prune_target.sh

# dev-docs/ is gitignored working state with no reviewer, CI or remote watching
# it grow. This gate FAILS above the size ceiling and NEVER deletes: which tier a
# file belongs in, and whether it is reproducible, is a judgement call. Entries
# past their tier lifetime are reported as a warning only (temp/bin churn is
# normal working state). Tier lifecycles: dev-docs/README.md.
DEV_DOCS_MAX_MB := 256
check-dev-docs:
	@[ -d dev-docs ] || { echo "no dev-docs/ - nothing to bound"; exit 0; }; \
	mb=$$(du -sm dev-docs | cut -f1); \
	stale=$$( { find dev-docs/bench/out -mindepth 1 -maxdepth 1 -mtime +14; \
	            find dev-docs/temp      -mindepth 1 -maxdepth 1 -mtime +1;  \
	            find dev-docs/bin       -mindepth 1 -maxdepth 1 -mtime +7;  \
	          } 2>/dev/null ); \
	if [ "$${mb:-0}" -ge $(DEV_DOCS_MAX_MB) ]; then \
		echo "FAIL: dev-docs/ is $${mb} MB (>= $(DEV_DOCS_MAX_MB) MB)"; \
		echo "  largest tiers:"; \
		du -sm dev-docs/* dev-docs/bench/* 2>/dev/null | sort -rn | head -8 | sed 's/^/    /'; \
		[ -z "$$stale" ] || { echo "  past their documented lifetime:"; echo "$$stale" | sed 's/^/    /'; }; \
		echo "  -> reclaim, or move anything irreproducible to a durable tier (dev-docs/README.md)"; \
		exit 1; \
	fi; \
	echo "dev-docs/ is $${mb} MB (limit $(DEV_DOCS_MAX_MB) MB)"; \
	[ -z "$$stale" ] || { echo "WARN: past their documented lifetime (dev-docs/README.md):"; \
	                      echo "$$stale" | sed 's/^/    /'; }
