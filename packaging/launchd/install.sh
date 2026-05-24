#!/bin/sh
set -eu

LABEL="com.dcchuck.car-go-clean"
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
TEMPLATE="$SCRIPT_DIR/$LABEL.plist"

BIN_PATH="${CAR_GO_CLEAN_BIN:-}"
if [ -z "$BIN_PATH" ]; then
  BIN_PATH=$(command -v car-go-clean || true)
fi
if [ -z "$BIN_PATH" ]; then
  echo "car-go-clean not found; set CAR_GO_CLEAN_BIN=/absolute/path/to/car-go-clean" >&2
  exit 1
fi

LOG_DIR="${CAR_GO_CLEAN_LOG_DIR:-$HOME/Library/Logs/car-go-clean}"
LAUNCH_AGENTS_DIR="$HOME/Library/LaunchAgents"
PLIST_PATH="$LAUNCH_AGENTS_DIR/$LABEL.plist"

escape_sed() {
  printf '%s' "$1" | sed 's/[&|]/\\&/g'
}

mkdir -p "$LOG_DIR" "$LAUNCH_AGENTS_DIR"
sed \
  -e "s|__CAR_GO_CLEAN_BIN__|$(escape_sed "$BIN_PATH")|g" \
  -e "s|__CAR_GO_CLEAN_LOG_DIR__|$(escape_sed "$LOG_DIR")|g" \
  "$TEMPLATE" > "$PLIST_PATH"

plutil -lint "$PLIST_PATH"
launchctl bootout "gui/$(id -u)" "$PLIST_PATH" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$(id -u)" "$PLIST_PATH"

echo "Installed $PLIST_PATH"
