SHELL := /bin/bash

CARGO ?= cargo
PREFIX ?= /usr
DESTDIR ?=
DESKTOP_TARGET ?= all
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

.PHONY: help run bench-ai fmt fmt-check lint test build install stage verify-install test-desktop release-build dist rpm ci clean

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
	@echo "  make test-desktop  Installed app gate: Fedora, Arch, obsidian-blue"
	@echo "  make verify-install PREFIX=/path/to/usr  Verify staged package"
	@echo "  make clean         Remove build and distribution outputs"

run:
	$(CARGO) run --locked

bench-ai:
	RUSTLE_BENCH_TIMEOUT_SECS=$(BENCH_TIMEOUT_SECS) $(CARGO) run --locked -p rustle-ai --features provider-bench --example provider_bench

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

lint:
	$(CARGO) clippy --locked --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --locked --workspace

build:
	RUSTFLAGS="-D warnings" $(CARGO) build --locked --workspace

install:
	$(CARGO) build --locked --bin "$(BIN)"
	install -Dm755 "$(TARGET_DIR)/debug/$(BIN)" "$(INSTALL_BIN_DIR)/$(BIN)"
	install -Dm644 "assets/icons/rustle.svg" "$(INSTALL_DATA_DIR)/icons/rustle.svg"
	install -Dm644 "assets/icons/rustle-recording.svg" "$(INSTALL_DATA_DIR)/icons/rustle-recording.svg"
	install -Dm644 "assets/icons/rustle.svg" "$(INSTALL_DATA_HOME)/icons/hicolor/scalable/apps/rustle.svg"
	install -Dm644 "assets/rustle.desktop" "$(INSTALL_DATA_HOME)/applications/rustle.desktop"
	@echo "Installed $(BIN) to $(INSTALL_BIN_DIR)/$(BIN)"
	@echo "Installed tray assets to $(INSTALL_DATA_DIR)/icons"

release-build:
	RUSTFLAGS="-D warnings" $(CARGO) build --locked --workspace --release


dist: release-build
	rm -rf "$(DIST_DIR)"
	mkdir -p "$(DIST_DIR)"
	$(MAKE) stage DESTDIR="$(abspath $(DIST_DIR))/root" PREFIX=/usr
	tar -czf "$(DIST_DIR)/$(BIN)-$(VERSION)-$(HOST).tar.gz" -C "$(DIST_DIR)/root" usr
	sha256sum "$(DIST_DIR)/$(BIN)-$(VERSION)-$(HOST).tar.gz" > "$(DIST_DIR)/$(BIN)-$(VERSION)-$(HOST).tar.gz.sha256"

rpm: release-build
	mkdir -p "$(DIST_DIR)"
	rm -f "$(DIST_DIR)"/$(BIN)-*.rpm "$(DIST_DIR)"/$(BIN)-*.rpm.sha256
	$(CARGO) generate-rpm -o "$(DIST_DIR)/$(BIN)-$(VERSION)-$(RPM_RELEASE).$(RPM_ARCH).rpm"
	sha256sum "$(DIST_DIR)/$(BIN)-$(VERSION)-$(RPM_RELEASE).$(RPM_ARCH).rpm" > "$(DIST_DIR)/$(BIN)-$(VERSION)-$(RPM_RELEASE).$(RPM_ARCH).rpm.sha256"

stage:
	install -Dm755 "$(RELEASE_BIN)" "$(DESTDIR)$(PREFIX)/bin/rustle"
	install -Dm644 assets/icons/rustle.svg "$(DESTDIR)$(PREFIX)/share/rustle/icons/rustle.svg"
	install -Dm644 assets/icons/rustle-recording.svg "$(DESTDIR)$(PREFIX)/share/rustle/icons/rustle-recording.svg"
	install -Dm644 assets/icons/rustle.svg "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/rustle.svg"
	install -Dm644 assets/rustle.desktop "$(DESTDIR)$(PREFIX)/share/applications/rustle.desktop"
	install -Dm644 assets/rustle.service "$(DESTDIR)$(PREFIX)/lib/systemd/user/rustle.service"

verify-install:
	python3 scripts/verify-install.py "$(DESTDIR)$(PREFIX)"

test-desktop:
	python3 scripts/test-desktop-vm.py "$(DESKTOP_TARGET)"

ci: fmt-check lint test build
	python3 -m unittest discover -s tests -p 'test_*.py'

clean:
	$(CARGO) clean
	rm -rf "$(DIST_DIR)"
