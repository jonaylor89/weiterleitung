.DEFAULT_GOAL := help

.PHONY: help run fmt fmt-check lint test check build release sqlx-prepare

help: ## Show available targets.
	@awk 'BEGIN { FS = ":.*##" } /^[a-zA-Z0-9_-]+:.*##/ { printf "%-14s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

run: ## Run the application with the local configuration.
	cargo run

fmt: ## Format Rust source files.
	cargo fmt

fmt-check: ## Check Rust source formatting.
	cargo fmt --check

lint: ## Run Clippy with warnings treated as errors.
	cargo clippy --all-targets -- -D warnings

test: ## Run the test suite.
	cargo test

check: fmt-check lint test ## Run all development checks.

build: ## Build the debug binary.
	cargo build

release: ## Build the release binary.
	cargo build --release

sqlx-prepare: ## Refresh SQLx offline query metadata.
	DATABASE_URL="sqlite://data/weiterleitung.db?mode=rwc" cargo sqlx prepare -- --all-targets
