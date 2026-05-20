SHELL := /bin/bash

CARGO ?= cargo
TARGET_DIR ?= target
DIST_DIR ?= dist
INSTALL_BIN_DIR ?= $(HOME)/.local/bin
BENCH_TIMEOUT_SECS ?= 30
BIN := rustle
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
HOST := $(shell rustc -vV | sed -n 's/^host: //p')

.PHONY: help run bench-ai fmt lint test build install release-build dist ci clean

help:
	@echo "Rustle DevOps targets:"
	@echo "  make run           Run the manual notes MVP"
	@echo "  make bench-ai      Benchmark llama.cpp GGUF models on this machine"
	@echo "                     Override timeout with BENCH_TIMEOUT_SECS=<seconds>"
	@echo "  make fmt           Check Rust formatting"
	@echo "  make lint          Run clippy with warnings denied"
	@echo "  make test          Run the full workspace test suite"
	@echo "  make build         Build the full workspace with warnings denied"
	@echo "  make install       Build and install the dev binary to INSTALL_BIN_DIR"
	@echo "  make release-build Build optimized release binaries"
	@echo "  make dist          Package the release binary and checksum"
	@echo "  make ci            Run fmt, lint, test, and build"
	@echo "  make clean         Remove build and distribution outputs"

run:
	$(CARGO) run

bench-ai:
	RUSTLE_BENCH_TIMEOUT_SECS=$(BENCH_TIMEOUT_SECS) $(CARGO) run -p rustle-ai --features provider-bench --example provider_bench

fmt:
	$(CARGO) fmt --all -- --check

lint:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace

build:
	RUSTFLAGS="-D warnings" $(CARGO) build --workspace

install:
	$(CARGO) build --bin "$(BIN)"
	install -Dm755 "$(TARGET_DIR)/debug/$(BIN)" "$(INSTALL_BIN_DIR)/$(BIN)"
	@echo "Installed $(BIN) to $(INSTALL_BIN_DIR)/$(BIN)"

release-build:
	RUSTFLAGS="-D warnings" $(CARGO) build --workspace --release

dist: release-build
	rm -rf "$(DIST_DIR)"
	mkdir -p "$(DIST_DIR)"
	tar -czf "$(DIST_DIR)/$(BIN)-$(VERSION)-$(HOST).tar.gz" -C "$(TARGET_DIR)/release" "$(BIN)"
	sha256sum "$(DIST_DIR)/$(BIN)-$(VERSION)-$(HOST).tar.gz" > "$(DIST_DIR)/$(BIN)-$(VERSION)-$(HOST).tar.gz.sha256"

ci: fmt lint test build

clean:
	$(CARGO) clean
	rm -rf "$(DIST_DIR)"
