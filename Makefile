CARGO ?= cargo

.PHONY: build test test-installer test-upgrade test-release-notes test-release-scripts test-msrv fmt clippy clean

build:
	$(CARGO) build

test: test-installer test-upgrade test-release-notes test-release-scripts test-msrv
	$(CARGO) test

test-installer:
	sh tests/installer.sh

test-upgrade:
	sh tests/upgrade.sh

test-release-notes:
	sh tests/release-notes.sh

test-release-scripts:
	sh tests/release-scripts.sh

test-msrv:
	sh tests/msrv.sh

fmt:
	$(CARGO) fmt -- --check

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

clean:
	$(CARGO) clean
