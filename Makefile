CARGO ?= cargo

.PHONY: build test fmt clippy clean

build:
	$(CARGO) build

test:
	$(CARGO) test

fmt:
	$(CARGO) fmt -- --check

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

clean:
	$(CARGO) clean
