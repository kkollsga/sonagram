.PHONY: check-free-space prune-target

check-free-space:
	@./scripts/check_free_space.sh

prune-target:
	@./scripts/prune_target.sh
