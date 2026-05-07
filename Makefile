.DEFAULT_GOAL := help

.PHONY: help
help: ## Show available make targets.
	@awk 'BEGIN { FS = ":.*##" } \
		/^## .* ##$$/ { \
			title = $$0; \
			gsub(/^## /, "", title); \
			gsub(/ ##$$/, "", title); \
			printf "\n\033[1m%s\033[0m\n", title; \
			next; \
		} \
		/^[a-zA-Z0-9][a-zA-Z0-9_-]+:.*##/ { \
			desc = $$2; \
			gsub(/^[ \t]+/, "", desc); \
			printf "  \033[36m%-24s\033[0m %s\n", $$1, desc; \
		}' $(MAKEFILE_LIST)

## Setup ##

.PHONY: setup-tools
setup-tools: ## Install dev tooling.
	cargo install cargo-llvm-cov cargo-deny --locked

## Format ##

.PHONY: fmt
fmt: rust-fmt desktop-fmt ## Format all code (Rust + frontend).

.PHONY: fmt-check
fmt-check: rust-fmt-check desktop-fmt-check ## Check formatting without modifying files.

.PHONY: rust-fmt
rust-fmt: ## Format Rust code.
	cargo fmt --all

.PHONY: rust-fmt-check
rust-fmt-check: ## Check Rust formatting.
	cargo fmt --all -- --check

.PHONY: desktop-fmt
desktop-fmt: ## Format frontend code (oxfmt).
	pnpm fmt

.PHONY: desktop-fmt-check
desktop-fmt-check: ## Check frontend formatting.
	pnpm fmt:check

## Lint ##

.PHONY: lint
lint: rust-lint desktop-lint ## Lint all code (clippy + oxlint).

.PHONY: rust-lint
rust-lint: ## Run clippy across the workspace with warnings escalated to errors.
	cargo clippy --workspace --all-targets -- -D warnings

.PHONY: desktop-lint
desktop-lint: ## Lint frontend (oxlint).
	pnpm lint

.PHONY: desktop-typecheck
desktop-typecheck: ## Type-check frontend (tsgo + svelte-check).
	pnpm typecheck
	pnpm check

## Test ##

.PHONY: test
test: rust-test desktop-test ## Run Rust + frontend tests.

.PHONY: rust-test
rust-test: ## Run workspace Rust tests.
	cargo test --workspace

.PHONY: rust-test-coverage
rust-test-coverage: ## Run workspace Rust tests with coverage and write lcov.info.
	cargo llvm-cov test --workspace --lcov --output-path lcov.info

.PHONY: desktop-test
desktop-test: ## Run frontend tests (vitest).
	pnpm test

.PHONY: desktop-test-coverage
desktop-test-coverage: ## Run frontend tests with coverage and write apps/desktop/coverage/lcov.info.
	pnpm --filter @nagori/desktop run test:coverage

## Build & Run ##

.PHONY: build
build: rust-build desktop-build ## Build Rust crates and the desktop app together.

.PHONY: rust-build
rust-build: ## Build the workspace in debug mode.
	cargo build --workspace

.PHONY: desktop-build
desktop-build: ## Bundle-build the desktop app (Tauri).
	pnpm --filter @nagori/desktop exec tauri build

.PHONY: run
run: ## Run the nagori daemon in the foreground.
	cargo run --bin nagori -- daemon run

.PHONY: dev-desktop
dev-desktop: ## Run the desktop app with hot reload (tauri dev).
	pnpm --filter @nagori/desktop exec tauri dev

.PHONY: clean
clean: ## Remove build artifacts.
	cargo clean
	rm -rf apps/desktop/dist

## Audit ##

.PHONY: deny-check
deny-check: ## Run cargo-deny across advisories, bans, licenses, and sources.
	cargo deny check all
