SHELL := /bin/bash

CARGO ?= cargo
TARGET_DIR ?= target
DIST_DIR ?= dist
INSTALL_BIN_DIR ?= $(HOME)/.local/bin
INSTALL_DATA_HOME ?= $(if $(XDG_DATA_HOME),$(XDG_DATA_HOME),$(HOME)/.local/share)
INSTALL_DATA_DIR ?= $(INSTALL_DATA_HOME)/rustle
BENCH_TIMEOUT_SECS ?= 30
BIN := rustle
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
HOST := $(shell rustc -vV | sed -n 's/^host: //p')
RPM_RELEASE ?= 1
RPM_ARCH := $(shell uname -m)
RELEASE_BIN := $(TARGET_DIR)/release/$(BIN)

.PHONY: help run bench-ai fmt lint test build install release-build dist rpm ci clean

help:
	@echo "Rustle DevOps targets:"
	@echo "  make run           Run the manual notes MVP"
	@echo "  make bench-ai      Benchmark llama.cpp GGUF models on this machine"
	@echo "                     Override timeout with BENCH_TIMEOUT_SECS=<seconds>"
	@echo "  make fmt           Check Rust formatting"
	@echo "  make lint          Run clippy with warnings denied"
	@echo "  make test          Run the full workspace test suite"
	@echo "  make build         Build the full workspace with warnings denied"
	@echo "  make install       Build and install the dev binary plus tray assets"
	@echo "  make release-build Build optimized release binaries"
	@echo "  make dist          Package the release binary and checksum"
	@echo "  make rpm           Package an RPM and checksum"
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
	install -Dm644 "assets/icons/rustle.svg" "$(INSTALL_DATA_DIR)/icons/rustle.svg"
	install -Dm644 "assets/icons/rustle-recording.svg" "$(INSTALL_DATA_DIR)/icons/rustle-recording.svg"
	@echo "Installed $(BIN) to $(INSTALL_BIN_DIR)/$(BIN)"
	@echo "Installed tray assets to $(INSTALL_DATA_DIR)/icons"

$(RELEASE_BIN):
	RUSTFLAGS="-D warnings" $(CARGO) build --workspace --release

release-build: $(RELEASE_BIN)

dist: $(RELEASE_BIN)
	rm -rf "$(DIST_DIR)"
	mkdir -p "$(DIST_DIR)"
	tar -czf "$(DIST_DIR)/$(BIN)-$(VERSION)-$(HOST).tar.gz" -C "$(TARGET_DIR)/release" "$(BIN)"
	sha256sum "$(DIST_DIR)/$(BIN)-$(VERSION)-$(HOST).tar.gz" > "$(DIST_DIR)/$(BIN)-$(VERSION)-$(HOST).tar.gz.sha256"

rpm: $(RELEASE_BIN)
	mkdir -p "$(DIST_DIR)"
	rm -f "$(DIST_DIR)"/$(BIN)-*.rpm "$(DIST_DIR)"/$(BIN)-*.rpm.sha256
	$(CARGO) generate-rpm -o "$(DIST_DIR)/$(BIN)-$(VERSION)-$(RPM_RELEASE).$(RPM_ARCH).rpm"
	sha256sum "$(DIST_DIR)/$(BIN)-$(VERSION)-$(RPM_RELEASE).$(RPM_ARCH).rpm" > "$(DIST_DIR)/$(BIN)-$(VERSION)-$(RPM_RELEASE).$(RPM_ARCH).rpm.sha256"

ci: fmt lint test build

clean:
	$(CARGO) clean
	rm -rf "$(DIST_DIR)"
