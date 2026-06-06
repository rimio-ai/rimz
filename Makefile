PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin

.PHONY: build install fmt lint test doctest deny deps vet coverage semver invariants ci

build:
	cargo xtask build

install:
	@if [ "$$(id -u)" -eq 0 ] && [ -n "$${SUDO_USER:-}" ]; then \
		set -eu; \
		user_home=$$(sudo -H -u "$$SUDO_USER" sh -c 'printf "%s" "$$HOME"'); \
		cargo_target_dir="$${CARGO_TARGET_DIR:-target}"; \
		stage_bin="$$cargo_target_dir/xtask/install/bin"; \
		dest="$(DESTDIR)$(BINDIR)"; \
		printf '%s\n' "building install artifacts as $$SUDO_USER"; \
		sudo -H -u "$$SUDO_USER" env PATH="$$user_home/.cargo/bin:$$PATH" CARGO_TARGET_DIR="$$cargo_target_dir" cargo xtask stage-install; \
		printf '%s\n' "installing rimz artifacts to $$dest"; \
		install -d "$$dest"; \
		name=rimz; \
		tmp="$$dest/.$$name.tmp.$$$$"; \
		install -m 0755 "$$stage_bin/$$name" "$$tmp"; \
		mv -f "$$tmp" "$$dest/$$name"; \
	else \
		cargo xtask install; \
	fi

fmt:
	cargo xtask fmt

lint:
	cargo xtask lint

test:
	cargo xtask test

doctest:
	cargo xtask doctest

deny:
	cargo xtask deny

deps:
	cargo xtask deps

vet:
	cargo xtask vet

coverage:
	cargo xtask coverage

semver:
	cargo xtask semver

invariants:
	cargo xtask invariants

ci:
	cargo xtask ci
