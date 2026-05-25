.PHONY: build install fmt lint test doctest deny deps vet coverage semver invariants ci

build:
	cargo xtask build

install:
	cargo xtask install

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
