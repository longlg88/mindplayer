# MindPlayer — convenience targets. Most users only need `make install`.
BIN := mindplayer
.DEFAULT_GOAL := help

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  make %-11s %s\n", $$1, $$2}'

install: ## Build + install the `mindplayer` TUI to ~/.local/bin
	./install.sh

app: ## Build the optional macOS app (.app/.dmg) — needs Node/npm
	./install.sh --app

build: ## Build the optimized TUI binary into ./target/release
	cargo build --release -p mindplayer-tui

run: build ## Build, then run the TUI in the current directory
	./target/release/$(BIN)

test: ## Run the test suite
	cargo test --all

uninstall: ## Remove the installed binary
	./install.sh --uninstall

.PHONY: help install app build run test uninstall
