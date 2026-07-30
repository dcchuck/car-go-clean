CARGO ?= cargo

.PHONY: build test test-installer test-upgrade test-release-notes test-msrv fmt clippy clean

build:
	$(CARGO) build

test: test-installer test-upgrade test-release-notes test-msrv
	$(CARGO) test

test-installer:
	sh tests/installer.sh

test-upgrade:
	sh tests/upgrade.sh

test-release-notes:
	sh tests/release-notes.sh

test-msrv:
	sh tests/msrv.sh

fmt:
	$(CARGO) fmt -- --check

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

clean:
	$(CARGO) clean
