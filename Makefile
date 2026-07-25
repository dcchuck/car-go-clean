CARGO ?= cargo

.PHONY: build test test-installer fmt clippy clean

build:
	$(CARGO) build

test: test-installer
	$(CARGO) test

test-installer:
	sh tests/installer.sh

fmt:
	$(CARGO) fmt -- --check

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

clean:
	$(CARGO) clean
