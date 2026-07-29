CARGO ?= cargo

.PHONY: build test test-installer test-release-notes fmt clippy clean

build:
	$(CARGO) build

test: test-installer test-release-notes
	$(CARGO) test

test-installer:
	sh tests/installer.sh

test-release-notes:
	sh tests/release-notes.sh

fmt:
	$(CARGO) fmt -- --check

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

clean:
	$(CARGO) clean
