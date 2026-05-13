GO        ?= go
PKG       := ./...
BIN       := car-go-clean
BIN_DIR   := bin

.PHONY: build test vet install clean

build:
	$(GO) build -o $(BIN_DIR)/$(BIN) ./cmd/car-go-clean

test:
	$(GO) test -race $(PKG)

vet:
	$(GO) vet $(PKG)

install:
	$(GO) install ./cmd/car-go-clean

clean:
	rm -rf $(BIN_DIR) dist
