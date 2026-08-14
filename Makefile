.DEFAULT_GOAL := help

CARGO ?= cargo

.PHONY: help install fmt fmt-check check test clippy build ci clean

help: ## Show available targets
	@awk 'BEGIN {FS = ":.*##"; printf "Usage: make <target>\n\nTargets:\n"} /^[a-zA-Z0-9_.-]+:.*##/ {printf "  %-12s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

install: ## Install the release binary
	$(CARGO) install --path tenet-cli --locked

fmt: ## Format Rust source files
	$(CARGO) fmt --all

fmt-check: ## Check Rust formatting without modifying files
	$(CARGO) fmt --all -- --check

check: ## Type-check all targets with every feature enabled
	$(CARGO) check --all-targets --all-features --locked

test: ## Run all tests with every feature enabled
	$(CARGO) test --all-targets --all-features --locked

clippy: ## Run Clippy with warnings treated as errors
	$(CARGO) clippy --all-targets --all-features --locked -- -D warnings

build: ## Build the release binary
	$(CARGO) build --release --locked

ci: fmt-check check clippy test ## Run the complete CI quality gate

clean: ## Remove Cargo build artifacts
	$(CARGO) clean
