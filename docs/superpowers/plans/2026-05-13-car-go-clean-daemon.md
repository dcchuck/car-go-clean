# car-go-clean Daemon Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Go daemon `car-go-clean` that periodically runs `cargo clean` on every Rust project it discovers under user-configured directories, tracks reclaimed disk space, and exposes health/status/stats via CLI subcommands.

**Architecture:** A single Go binary with subcommands. The `daemon` subcommand is long-running (launched by `launchd`/`systemd`/`brew services`) and owns a SQLite state file. All other subcommands are short-lived utilities that read (and, for `scan`/`run`, write — under an advisory lock) the same state file. Components are packaged behind small interfaces (`FS`, `CommandRunner`, `Clock`) so they can be unit-tested in isolation.

**Tech Stack:** Go 1.22+, `modernc.org/sqlite` (pure-Go, no CGO), `github.com/BurntSushi/toml`, `github.com/spf13/cobra`, `gopkg.in/natefinch/lumberjack.v2`, `golang.org/x/sys/unix` (flock). Tests use the stdlib `testing` package and table-driven patterns.

**Spec:** `docs/superpowers/specs/2026-05-13-car-go-clean-daemon-design.md`

**Conventions:**
- TDD throughout: failing test → minimal code → green → commit.
- Frequent small commits, one logical change per commit.
- File paths in this plan are **absolute file paths from the repo root**.
- All shell commands assume CWD is the repo root.

---

## Chunk 1: Bootstrap and Config Package

This chunk gets the project to a state where `go build ./...` and `go test ./...` succeed and the `internal/config` package can load and validate TOML configuration with sensible defaults.

### Task 1: Initialize Go module and repo skeleton

**Files:**
- Create: `go.mod`
- Create: `.gitignore`
- Create: `Makefile`
- Create: `README.md`
- Create: `cmd/car-go-clean/main.go`

- [ ] **Step 1: Initialize the module**

Run:
```bash
go mod init github.com/dcchuck/car-go-clean
```

Expected: creates `go.mod` with one line `module github.com/dcchuck/car-go-clean` plus the `go` directive.

- [ ] **Step 2: Add `.gitignore`**

Write to `.gitignore`:
```
# Build artifacts
/dist/
/bin/
*.test
*.out

# Editor / OS
.idea/
.vscode/
.DS_Store

# Local state during dev
*.db
*.log
```

- [ ] **Step 3: Add a minimal `cmd/car-go-clean/main.go`**

```go
package main

import "fmt"

func main() {
    fmt.Println("car-go-clean (skeleton)")
}
```

- [ ] **Step 4: Add the `Makefile`**

```make
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
```

- [ ] **Step 5: Add a one-paragraph `README.md`**

```markdown
# car-go-clean

A daemon that periodically runs `cargo clean` against every Rust project it
finds in your configured directories, tracking reclaimed disk space.

See `docs/superpowers/specs/` for the design.
```

- [ ] **Step 6: Verify the skeleton builds**

Run:
```bash
make build
./bin/car-go-clean
```

Expected first command: exits 0, produces `bin/car-go-clean`.
Expected second command: prints `car-go-clean (skeleton)`.

- [ ] **Step 7: Commit**

```bash
git add go.mod .gitignore Makefile README.md cmd/car-go-clean/main.go
git commit -m "chore: initialize Go module and repo skeleton"
```

---

### Task 2: Add config types and defaults

**Files:**
- Create: `internal/config/config.go`
- Create: `internal/config/config_test.go`

- [ ] **Step 1: Write a failing test for `Default()`**

`internal/config/config_test.go`:
```go
package config

import (
    "os"
    "testing"
    "time"
)

func TestDefault_ScanDirsIsHome(t *testing.T) {
    home, err := os.UserHomeDir()
    if err != nil {
        t.Fatalf("UserHomeDir: %v", err)
    }
    cfg := Default()
    if len(cfg.ScanDirs) != 1 || cfg.ScanDirs[0] != home {
        t.Fatalf("ScanDirs = %v, want [%s]", cfg.ScanDirs, home)
    }
    if len(cfg.ProjectDirs) != 0 {
        t.Fatalf("ProjectDirs = %v, want []", cfg.ProjectDirs)
    }
    if cfg.CleanInterval != 24*time.Hour {
        t.Fatalf("CleanInterval = %v, want 24h", cfg.CleanInterval)
    }
    if cfg.ScanInterval != 168*time.Hour {
        t.Fatalf("ScanInterval = %v, want 168h", cfg.ScanInterval)
    }
    if cfg.LogLevel != "info" {
        t.Fatalf("LogLevel = %q, want info", cfg.LogLevel)
    }
    if len(cfg.Excludes) == 0 {
        t.Fatalf("Excludes = empty, want non-empty default")
    }
}
```

- [ ] **Step 2: Run the test — must fail with "undefined: Default"**

```bash
go test ./internal/config/...
```

Expected: FAIL — `undefined: config.Default`.

- [ ] **Step 3: Implement `Config` and `Default()`**

`internal/config/config.go`:
```go
// Package config loads and validates the user's configuration file.
package config

import (
    "os"
    "time"
)

type Config struct {
    ScanDirs      []string      `toml:"scan_dirs"`
    ProjectDirs   []string      `toml:"project_dirs"`
    Excludes      []string      `toml:"excludes"`
    CleanInterval time.Duration `toml:"clean_interval"`
    ScanInterval  time.Duration `toml:"scan_interval"`
    LogLevel      string        `toml:"log_level"`
}

func defaultExcludes() []string {
    return []string{
        ".git",
        "node_modules",
        ".cargo",
        ".rustup",
        "target",
        "Library/Caches",
    }
}

func Default() Config {
    home, _ := os.UserHomeDir()
    var scan []string
    if home != "" {
        scan = []string{home}
    }
    return Config{
        ScanDirs:      scan,
        ProjectDirs:   []string{},
        Excludes:      defaultExcludes(),
        CleanInterval: 24 * time.Hour,
        ScanInterval:  168 * time.Hour,
        LogLevel:      "info",
    }
}
```

- [ ] **Step 4: Run the test — must pass**

```bash
go test ./internal/config/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/config/
git commit -m "feat(config): add Config struct and Default()"
```

---

### Task 3: Load config from TOML file with defaults merged

**Files:**
- Modify: `go.mod` (add TOML dep)
- Modify: `internal/config/config.go`
- Modify: `internal/config/config_test.go`

- [ ] **Step 1: Add the TOML dependency**

```bash
go get github.com/BurntSushi/toml@latest
go mod tidy
```

- [ ] **Step 2: Write a failing test for `Load`**

Append to `internal/config/config_test.go`:
```go
func TestLoad_FileMissingReturnsDefaults(t *testing.T) {
    cfg, err := Load("/nonexistent/path/config.toml")
    if err != nil {
        t.Fatalf("Load: %v", err)
    }
    if cfg.CleanInterval != 24*time.Hour {
        t.Fatalf("CleanInterval = %v, want default 24h", cfg.CleanInterval)
    }
}

func TestLoad_FileOverridesDefaults(t *testing.T) {
    dir := t.TempDir()
    path := dir + "/config.toml"
    err := os.WriteFile(path, []byte(`
scan_dirs    = ["/tmp/a", "/tmp/b"]
project_dirs = ["/tmp/p"]
clean_interval = "1h"
scan_interval  = "2h"
log_level      = "debug"
excludes       = ["foo"]
`), 0o644)
    if err != nil {
        t.Fatal(err)
    }
    cfg, err := Load(path)
    if err != nil {
        t.Fatalf("Load: %v", err)
    }
    if got := cfg.ScanDirs; len(got) != 2 || got[0] != "/tmp/a" || got[1] != "/tmp/b" {
        t.Fatalf("ScanDirs = %v", got)
    }
    if cfg.CleanInterval != time.Hour {
        t.Fatalf("CleanInterval = %v, want 1h", cfg.CleanInterval)
    }
    if cfg.LogLevel != "debug" {
        t.Fatalf("LogLevel = %q", cfg.LogLevel)
    }
}
```

- [ ] **Step 3: Run the tests — must fail with "undefined: Load"**

```bash
go test ./internal/config/...
```

Expected: FAIL.

- [ ] **Step 4: Implement `Load`**

Append to `internal/config/config.go`:
```go
import (
    "errors"
    "os"
    "path/filepath"
    "strings"
    "time"

    "github.com/BurntSushi/toml"
)

// Load returns Default() if path does not exist. Otherwise it parses the
// file at path and overlays the parsed values onto the defaults: any field
// missing from the file keeps its default value.
func Load(path string) (Config, error) {
    cfg := Default()
    f, err := os.Open(path)
    if err != nil {
        if errors.Is(err, os.ErrNotExist) {
            return cfg, nil
        }
        return Config{}, err
    }
    defer f.Close()

    if _, err := toml.NewDecoder(f).Decode(&cfg); err != nil {
        return Config{}, err
    }
    cfg.ScanDirs = expandAll(cfg.ScanDirs)
    cfg.ProjectDirs = expandAll(cfg.ProjectDirs)
    return cfg, nil
}

func expandAll(paths []string) []string {
    out := make([]string, 0, len(paths))
    for _, p := range paths {
        out = append(out, expand(p))
    }
    return out
}

func expand(p string) string {
    p = os.ExpandEnv(p)
    if strings.HasPrefix(p, "~") {
        if home, err := os.UserHomeDir(); err == nil {
            p = filepath.Join(home, strings.TrimPrefix(p, "~"))
        }
    }
    return p
}
```

(Move the existing `import` block to merge with the new imports.)

- [ ] **Step 5: Run the tests — must pass**

```bash
go test ./internal/config/...
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add go.mod go.sum internal/config/
git commit -m "feat(config): load TOML config with defaults overlay"
```

---

### Task 4: Add `DefaultPath()` for the standard config location

**Files:**
- Modify: `internal/config/config.go`
- Modify: `internal/config/config_test.go`

- [ ] **Step 1: Write failing test for `DefaultPath`**

Append to `internal/config/config_test.go`:
```go
func TestDefaultPath_XDGConfigHomeRespected(t *testing.T) {
    t.Setenv("XDG_CONFIG_HOME", "/tmp/xdg")
    got := DefaultPath()
    want := "/tmp/xdg/car-go-clean/config.toml"
    if got != want {
        t.Fatalf("DefaultPath = %q, want %q", got, want)
    }
}

func TestDefaultPath_FallsBackToHome(t *testing.T) {
    t.Setenv("XDG_CONFIG_HOME", "")
    got := DefaultPath()
    home, _ := os.UserHomeDir()
    want := home + "/.config/car-go-clean/config.toml"
    if got != want {
        t.Fatalf("DefaultPath = %q, want %q", got, want)
    }
}
```

- [ ] **Step 2: Run tests — must fail**

```bash
go test ./internal/config/...
```

Expected: FAIL — `undefined: DefaultPath`.

- [ ] **Step 3: Implement `DefaultPath`**

Append to `internal/config/config.go`:
```go
// DefaultPath returns the standard config file location:
// $XDG_CONFIG_HOME/car-go-clean/config.toml, falling back to
// $HOME/.config/car-go-clean/config.toml.
func DefaultPath() string {
    if xdg := os.Getenv("XDG_CONFIG_HOME"); xdg != "" {
        return filepath.Join(xdg, "car-go-clean", "config.toml")
    }
    home, _ := os.UserHomeDir()
    return filepath.Join(home, ".config", "car-go-clean", "config.toml")
}
```

- [ ] **Step 4: Run tests — must pass**

```bash
go test ./internal/config/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/config/
git commit -m "feat(config): add DefaultPath() with XDG support"
```

---

### Task 5: Add `Validate()` that catches misconfigurations

**Files:**
- Modify: `internal/config/config.go`
- Modify: `internal/config/config_test.go`

`Validate()` is **structural** validation (intervals positive, log level recognized) — it does NOT touch the filesystem. Directory existence is the `health` command's job (per spec). This separation keeps `Load` cheap and side-effect-free.

- [ ] **Step 1: Failing test for `Validate`**

Append to `internal/config/config_test.go`:
```go
func TestValidate_RejectsNonPositiveCleanInterval(t *testing.T) {
    cfg := Default()
    cfg.CleanInterval = 0
    if err := cfg.Validate(); err == nil {
        t.Fatal("Validate: expected error for zero CleanInterval")
    }
}

func TestValidate_RejectsNonPositiveScanInterval(t *testing.T) {
    cfg := Default()
    cfg.ScanInterval = 0
    if err := cfg.Validate(); err == nil {
        t.Fatal("Validate: expected error for zero ScanInterval")
    }
}

func TestValidate_RejectsUnknownLogLevel(t *testing.T) {
    cfg := Default()
    cfg.LogLevel = "verbose"
    if err := cfg.Validate(); err == nil {
        t.Fatal("Validate: expected error for unknown log level")
    }
}

func TestValidate_AcceptsDefaults(t *testing.T) {
    if err := Default().Validate(); err != nil {
        t.Fatalf("Validate(Default()) = %v, want nil", err)
    }
}
```

- [ ] **Step 2: Run tests — must fail**

```bash
go test ./internal/config/...
```

Expected: FAIL — `Validate` undefined.

- [ ] **Step 3: Implement `Validate`**

Append to `internal/config/config.go`:
```go
import (
    "fmt"
)

func (c Config) Validate() error {
    if c.CleanInterval <= 0 {
        return fmt.Errorf("clean_interval must be positive, got %s", c.CleanInterval)
    }
    if c.ScanInterval <= 0 {
        return fmt.Errorf("scan_interval must be positive, got %s", c.ScanInterval)
    }
    switch c.LogLevel {
    case "debug", "info", "warn", "error":
    default:
        return fmt.Errorf("log_level %q: must be one of debug,info,warn,error", c.LogLevel)
    }
    return nil
}
```

(Merge the new `fmt` import with existing imports.)

- [ ] **Step 4: Run tests — must pass**

```bash
go test ./internal/config/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/config/
git commit -m "feat(config): add Validate() for structural config checks"
```

---

### Task 6: Add `Paths` helper for state directory

**Files:**
- Create: `internal/config/paths.go`
- Create: `internal/config/paths_test.go`

The daemon and CLI both need to compute the state-dir, db path, log path, and lock path. Centralize that in one place.

- [ ] **Step 1: Failing tests**

`internal/config/paths_test.go`:
```go
package config

import (
    "os"
    "path/filepath"
    "testing"
)

func TestPaths_XDGStateHomeRespected(t *testing.T) {
    t.Setenv("XDG_STATE_HOME", "/tmp/xdg-state")
    p := Paths()
    if p.StateDir != "/tmp/xdg-state/car-go-clean" {
        t.Fatalf("StateDir = %q", p.StateDir)
    }
    if p.DBPath != "/tmp/xdg-state/car-go-clean/state.db" {
        t.Fatalf("DBPath = %q", p.DBPath)
    }
    if p.LogPath != "/tmp/xdg-state/car-go-clean/car-go-clean.log" {
        t.Fatalf("LogPath = %q", p.LogPath)
    }
    if p.LockPath != "/tmp/xdg-state/car-go-clean/daemon.lock" {
        t.Fatalf("LockPath = %q", p.LockPath)
    }
}

func TestPaths_FallsBackToHome(t *testing.T) {
    t.Setenv("XDG_STATE_HOME", "")
    p := Paths()
    home, _ := os.UserHomeDir()
    want := filepath.Join(home, ".local", "state", "car-go-clean")
    if p.StateDir != want {
        t.Fatalf("StateDir = %q, want %q", p.StateDir, want)
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./internal/config/...
```

Expected: FAIL — `undefined: Paths`.

- [ ] **Step 3: Implement `Paths`**

`internal/config/paths.go`:
```go
package config

import (
    "os"
    "path/filepath"
)

type PathSet struct {
    StateDir string
    DBPath   string
    LogPath  string
    LockPath string
}

func Paths() PathSet {
    var stateDir string
    if xdg := os.Getenv("XDG_STATE_HOME"); xdg != "" {
        stateDir = filepath.Join(xdg, "car-go-clean")
    } else {
        home, _ := os.UserHomeDir()
        stateDir = filepath.Join(home, ".local", "state", "car-go-clean")
    }
    return PathSet{
        StateDir: stateDir,
        DBPath:   filepath.Join(stateDir, "state.db"),
        LogPath:  filepath.Join(stateDir, "car-go-clean.log"),
        LockPath: filepath.Join(stateDir, "daemon.lock"),
    }
}
```

- [ ] **Step 4: Run — must pass**

```bash
go test ./internal/config/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/config/
git commit -m "feat(config): add Paths() helper for state-dir layout"
```

---

### Task 6.5: End-of-chunk verification

- [ ] **Step 1: Build everything**

```bash
go build ./...
```

Expected: clean exit, no errors.

- [ ] **Step 2: Run the whole test suite with race detector**

```bash
go test -race ./...
```

Expected: PASS for every package created in this chunk.

- [ ] **Step 3: Run go vet**

```bash
go vet ./...
```

Expected: no warnings.

(No commit — these are read-only verifications.)

---

## Chunk 2: Store (SQLite) and Cache Packages

This chunk produces an embedded SQLite store backed by `modernc.org/sqlite` with migrations, plus a thin `cache` package layered on top of the `projects` table.

### Task 7: Add SQLite dependency and an open helper

**Files:**
- Modify: `go.mod`
- Create: `internal/store/store.go`
- Create: `internal/store/store_test.go`

- [ ] **Step 1: Add dependency**

```bash
go get modernc.org/sqlite@latest
go mod tidy
```

- [ ] **Step 2: Failing test — Open creates the file and pings**

`internal/store/store_test.go`:
```go
package store

import (
    "context"
    "path/filepath"
    "testing"
)

func TestOpen_CreatesFileAndPings(t *testing.T) {
    path := filepath.Join(t.TempDir(), "state.db")
    s, err := Open(context.Background(), path)
    if err != nil {
        t.Fatalf("Open: %v", err)
    }
    defer s.Close()
    if err := s.Ping(context.Background()); err != nil {
        t.Fatalf("Ping: %v", err)
    }
}
```

- [ ] **Step 3: Run — must fail**

```bash
go test ./internal/store/...
```

Expected: FAIL — `undefined: store.Open`.

- [ ] **Step 4: Implement `Open`**

`internal/store/store.go`:
```go
// Package store is the SQLite-backed persistence layer for car-go-clean.
package store

import (
    "context"
    "database/sql"
    "os"
    "path/filepath"

    _ "modernc.org/sqlite"
)

type Store struct {
    db *sql.DB
}

func Open(ctx context.Context, path string) (*Store, error) {
    if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
        return nil, err
    }
    db, err := sql.Open("sqlite", path+"?_pragma=journal_mode(WAL)&_pragma=busy_timeout(5000)")
    if err != nil {
        return nil, err
    }
    if err := db.PingContext(ctx); err != nil {
        db.Close()
        return nil, err
    }
    return &Store{db: db}, nil
}

func (s *Store) Close() error              { return s.db.Close() }
func (s *Store) Ping(ctx context.Context) error { return s.db.PingContext(ctx) }
func (s *Store) DB() *sql.DB               { return s.db }
```

- [ ] **Step 5: Run — must pass**

```bash
go test ./internal/store/...
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add go.mod go.sum internal/store/
git commit -m "feat(store): add Open/Close/Ping with SQLite WAL"
```

---

### Task 8: Add migrations runner

**Files:**
- Create: `internal/store/migrations.go`
- Modify: `internal/store/store.go`
- Modify: `internal/store/store_test.go`

- [ ] **Step 1: Failing test for `Migrate`**

Append to `internal/store/store_test.go`:
```go
func TestMigrate_CreatesTables(t *testing.T) {
    path := filepath.Join(t.TempDir(), "state.db")
    s, err := Open(context.Background(), path)
    if err != nil {
        t.Fatalf("Open: %v", err)
    }
    defer s.Close()
    if err := s.Migrate(context.Background()); err != nil {
        t.Fatalf("Migrate: %v", err)
    }
    tables := []string{"projects", "clean_events", "errors", "runs", "schema_version"}
    for _, name := range tables {
        var got string
        err := s.DB().QueryRowContext(context.Background(),
            `SELECT name FROM sqlite_master WHERE type='table' AND name=?`, name).Scan(&got)
        if err != nil {
            t.Errorf("table %q missing: %v", name, err)
        }
    }
}

func TestMigrate_Idempotent(t *testing.T) {
    path := filepath.Join(t.TempDir(), "state.db")
    s, _ := Open(context.Background(), path)
    defer s.Close()
    if err := s.Migrate(context.Background()); err != nil {
        t.Fatalf("first Migrate: %v", err)
    }
    if err := s.Migrate(context.Background()); err != nil {
        t.Fatalf("second Migrate: %v", err)
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./internal/store/...
```

Expected: FAIL.

- [ ] **Step 3: Implement migrations**

`internal/store/migrations.go`:
```go
package store

import "context"

var migrations = []string{
    // 1: initial schema
    `CREATE TABLE projects (
        path             TEXT PRIMARY KEY,
        discovered_at    TIMESTAMP NOT NULL,
        last_seen_at     TIMESTAMP NOT NULL,
        last_cleaned_at  TIMESTAMP
    );
    CREATE TABLE runs (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        started_at        TIMESTAMP NOT NULL,
        finished_at       TIMESTAMP,
        projects_cleaned  INTEGER NOT NULL DEFAULT 0,
        bytes_recovered   INTEGER NOT NULL DEFAULT 0,
        errors_count      INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE clean_events (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id          INTEGER NOT NULL REFERENCES runs(id),
        ts              TIMESTAMP NOT NULL,
        path            TEXT NOT NULL,
        bytes_before    INTEGER NOT NULL,
        bytes_after     INTEGER NOT NULL,
        duration_ms     INTEGER NOT NULL,
        exit_code       INTEGER NOT NULL,
        stderr_excerpt  TEXT
    );
    CREATE TABLE errors (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        ts        TIMESTAMP NOT NULL,
        category  TEXT NOT NULL,
        path      TEXT,
        message   TEXT NOT NULL
    );
    CREATE INDEX idx_clean_events_ts ON clean_events(ts);
    CREATE INDEX idx_errors_ts ON errors(ts);
    CREATE INDEX idx_runs_started_at ON runs(started_at);`,
}

func (s *Store) Migrate(ctx context.Context) error {
    if _, err := s.db.ExecContext(ctx,
        `CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)`); err != nil {
        return err
    }
    var current int
    err := s.db.QueryRowContext(ctx, `SELECT COALESCE(MAX(version), 0) FROM schema_version`).Scan(&current)
    if err != nil {
        return err
    }
    for i := current; i < len(migrations); i++ {
        tx, err := s.db.BeginTx(ctx, nil)
        if err != nil {
            return err
        }
        if _, err := tx.ExecContext(ctx, migrations[i]); err != nil {
            tx.Rollback()
            return err
        }
        if _, err := tx.ExecContext(ctx, `INSERT INTO schema_version (version) VALUES (?)`, i+1); err != nil {
            tx.Rollback()
            return err
        }
        if err := tx.Commit(); err != nil {
            return err
        }
    }
    return nil
}
```

- [ ] **Step 4: Run — must pass**

```bash
go test ./internal/store/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/store/
git commit -m "feat(store): add idempotent SQL migrations"
```

---

### Task 9: Add typed accessors for `projects` table

**Files:**
- Create: `internal/store/projects.go`
- Create: `internal/store/projects_test.go`

- [ ] **Step 1: Failing test**

`internal/store/projects_test.go`:
```go
package store

import (
    "context"
    "path/filepath"
    "testing"
    "time"
)

func newTestStore(t *testing.T) *Store {
    t.Helper()
    path := filepath.Join(t.TempDir(), "state.db")
    s, err := Open(context.Background(), path)
    if err != nil {
        t.Fatalf("Open: %v", err)
    }
    if err := s.Migrate(context.Background()); err != nil {
        t.Fatalf("Migrate: %v", err)
    }
    t.Cleanup(func() { s.Close() })
    return s
}

func TestUpsertProject_InsertsThenUpdatesLastSeen(t *testing.T) {
    s := newTestStore(t)
    ctx := context.Background()
    t0 := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
    t1 := t0.Add(time.Hour)
    if err := s.UpsertProject(ctx, "/a", t0); err != nil {
        t.Fatalf("first upsert: %v", err)
    }
    if err := s.UpsertProject(ctx, "/a", t1); err != nil {
        t.Fatalf("second upsert: %v", err)
    }
    ps, err := s.AllProjects(ctx)
    if err != nil {
        t.Fatalf("AllProjects: %v", err)
    }
    if len(ps) != 1 {
        t.Fatalf("got %d projects, want 1", len(ps))
    }
    if !ps[0].DiscoveredAt.Equal(t0) {
        t.Errorf("DiscoveredAt = %v, want %v", ps[0].DiscoveredAt, t0)
    }
    if !ps[0].LastSeenAt.Equal(t1) {
        t.Errorf("LastSeenAt = %v, want %v", ps[0].LastSeenAt, t1)
    }
}

func TestRemoveProject(t *testing.T) {
    s := newTestStore(t)
    ctx := context.Background()
    _ = s.UpsertProject(ctx, "/a", time.Now())
    _ = s.UpsertProject(ctx, "/b", time.Now())
    if err := s.RemoveProject(ctx, "/a"); err != nil {
        t.Fatalf("RemoveProject: %v", err)
    }
    ps, _ := s.AllProjects(ctx)
    if len(ps) != 1 || ps[0].Path != "/b" {
        t.Fatalf("after remove: %v", ps)
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./internal/store/...
```

Expected: FAIL.

- [ ] **Step 3: Implement**

`internal/store/projects.go`:
```go
package store

import (
    "context"
    "time"
)

type Project struct {
    Path           string
    DiscoveredAt   time.Time
    LastSeenAt     time.Time
    LastCleanedAt  *time.Time
}

func (s *Store) UpsertProject(ctx context.Context, path string, now time.Time) error {
    _, err := s.db.ExecContext(ctx, `
        INSERT INTO projects (path, discovered_at, last_seen_at)
        VALUES (?, ?, ?)
        ON CONFLICT(path) DO UPDATE SET last_seen_at = excluded.last_seen_at
    `, path, now, now)
    return err
}

func (s *Store) RemoveProject(ctx context.Context, path string) error {
    _, err := s.db.ExecContext(ctx, `DELETE FROM projects WHERE path = ?`, path)
    return err
}

func (s *Store) MarkProjectCleaned(ctx context.Context, path string, when time.Time) error {
    _, err := s.db.ExecContext(ctx, `UPDATE projects SET last_cleaned_at = ? WHERE path = ?`, when, path)
    return err
}

func (s *Store) AllProjects(ctx context.Context) ([]Project, error) {
    rows, err := s.db.QueryContext(ctx, `
        SELECT path, discovered_at, last_seen_at, last_cleaned_at
        FROM projects ORDER BY path`)
    if err != nil {
        return nil, err
    }
    defer rows.Close()
    var out []Project
    for rows.Next() {
        var p Project
        if err := rows.Scan(&p.Path, &p.DiscoveredAt, &p.LastSeenAt, &p.LastCleanedAt); err != nil {
            return nil, err
        }
        out = append(out, p)
    }
    return out, rows.Err()
}
```

- [ ] **Step 4: Run — must pass**

```bash
go test ./internal/store/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/store/
git commit -m "feat(store): add typed accessors for projects table"
```

---

### Task 10: Add accessors for `runs`, `clean_events`, `errors`

**Files:**
- Create: `internal/store/runs.go`
- Create: `internal/store/runs_test.go`

- [ ] **Step 1: Failing test for run lifecycle**

`internal/store/runs_test.go`:
```go
package store

import (
    "context"
    "testing"
    "time"
)

func TestStartFinishRun(t *testing.T) {
    s := newTestStore(t)
    ctx := context.Background()
    started := time.Now().UTC().Truncate(time.Second)
    id, err := s.StartRun(ctx, started)
    if err != nil {
        t.Fatalf("StartRun: %v", err)
    }
    if id <= 0 {
        t.Fatalf("StartRun id = %d", id)
    }
    finished := started.Add(time.Minute)
    if err := s.FinishRun(ctx, id, finished, 3, 100, 1); err != nil {
        t.Fatalf("FinishRun: %v", err)
    }
    r, err := s.GetRun(ctx, id)
    if err != nil {
        t.Fatalf("GetRun: %v", err)
    }
    if !r.StartedAt.Equal(started) || r.FinishedAt == nil || !r.FinishedAt.Equal(finished) {
        t.Errorf("times: %+v", r)
    }
    if r.ProjectsCleaned != 3 || r.BytesRecovered != 100 || r.ErrorsCount != 1 {
        t.Errorf("counters: %+v", r)
    }
}

func TestRecordCleanEvent(t *testing.T) {
    s := newTestStore(t)
    ctx := context.Background()
    runID, _ := s.StartRun(ctx, time.Now())
    err := s.RecordCleanEvent(ctx, CleanEvent{
        RunID:         runID,
        TS:            time.Now(),
        Path:          "/p",
        BytesBefore:   1000,
        BytesAfter:    0,
        DurationMS:    42,
        ExitCode:      0,
        StderrExcerpt: "",
    })
    if err != nil {
        t.Fatalf("RecordCleanEvent: %v", err)
    }
    events, err := s.CleanEventsSince(ctx, time.Time{})
    if err != nil {
        t.Fatalf("CleanEventsSince: %v", err)
    }
    if len(events) != 1 || events[0].Path != "/p" || events[0].BytesBefore != 1000 {
        t.Errorf("events: %+v", events)
    }
}

func TestRecordError(t *testing.T) {
    s := newTestStore(t)
    ctx := context.Background()
    err := s.RecordError(ctx, ErrorRecord{
        TS:       time.Now(),
        Category: "scan",
        Path:     "/x",
        Message:  "boom",
    })
    if err != nil {
        t.Fatalf("RecordError: %v", err)
    }
    errs, err := s.ErrorsSince(ctx, time.Time{})
    if err != nil {
        t.Fatalf("ErrorsSince: %v", err)
    }
    if len(errs) != 1 || errs[0].Category != "scan" || errs[0].Message != "boom" {
        t.Errorf("errors: %+v", errs)
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./internal/store/...
```

Expected: FAIL.

- [ ] **Step 3: Implement**

`internal/store/runs.go`:
```go
package store

import (
    "context"
    "time"
)

type Run struct {
    ID              int64
    StartedAt       time.Time
    FinishedAt      *time.Time
    ProjectsCleaned int64
    BytesRecovered  int64
    ErrorsCount     int64
}

type CleanEvent struct {
    ID            int64
    RunID         int64
    TS            time.Time
    Path          string
    BytesBefore   int64
    BytesAfter    int64
    DurationMS    int64
    ExitCode      int
    StderrExcerpt string
}

type ErrorRecord struct {
    ID       int64
    TS       time.Time
    Category string
    Path     string
    Message  string
}

func (s *Store) StartRun(ctx context.Context, startedAt time.Time) (int64, error) {
    res, err := s.db.ExecContext(ctx, `INSERT INTO runs (started_at) VALUES (?)`, startedAt)
    if err != nil {
        return 0, err
    }
    return res.LastInsertId()
}

func (s *Store) FinishRun(ctx context.Context, id int64, finishedAt time.Time, projectsCleaned, bytesRecovered, errorsCount int64) error {
    _, err := s.db.ExecContext(ctx, `
        UPDATE runs SET finished_at = ?, projects_cleaned = ?, bytes_recovered = ?, errors_count = ?
        WHERE id = ?`, finishedAt, projectsCleaned, bytesRecovered, errorsCount, id)
    return err
}

func (s *Store) GetRun(ctx context.Context, id int64) (Run, error) {
    var r Run
    err := s.db.QueryRowContext(ctx, `
        SELECT id, started_at, finished_at, projects_cleaned, bytes_recovered, errors_count
        FROM runs WHERE id = ?`, id).
        Scan(&r.ID, &r.StartedAt, &r.FinishedAt, &r.ProjectsCleaned, &r.BytesRecovered, &r.ErrorsCount)
    return r, err
}

func (s *Store) LastRun(ctx context.Context) (Run, error) {
    var r Run
    err := s.db.QueryRowContext(ctx, `
        SELECT id, started_at, finished_at, projects_cleaned, bytes_recovered, errors_count
        FROM runs ORDER BY started_at DESC LIMIT 1`).
        Scan(&r.ID, &r.StartedAt, &r.FinishedAt, &r.ProjectsCleaned, &r.BytesRecovered, &r.ErrorsCount)
    return r, err
}

func (s *Store) RecordCleanEvent(ctx context.Context, e CleanEvent) error {
    _, err := s.db.ExecContext(ctx, `
        INSERT INTO clean_events (run_id, ts, path, bytes_before, bytes_after, duration_ms, exit_code, stderr_excerpt)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    `, e.RunID, e.TS, e.Path, e.BytesBefore, e.BytesAfter, e.DurationMS, e.ExitCode, e.StderrExcerpt)
    return err
}

func (s *Store) CleanEventsSince(ctx context.Context, since time.Time) ([]CleanEvent, error) {
    rows, err := s.db.QueryContext(ctx, `
        SELECT id, run_id, ts, path, bytes_before, bytes_after, duration_ms, exit_code, stderr_excerpt
        FROM clean_events WHERE ts >= ? ORDER BY ts`, since)
    if err != nil {
        return nil, err
    }
    defer rows.Close()
    var out []CleanEvent
    for rows.Next() {
        var e CleanEvent
        if err := rows.Scan(&e.ID, &e.RunID, &e.TS, &e.Path, &e.BytesBefore, &e.BytesAfter, &e.DurationMS, &e.ExitCode, &e.StderrExcerpt); err != nil {
            return nil, err
        }
        out = append(out, e)
    }
    return out, rows.Err()
}

func (s *Store) RecordError(ctx context.Context, e ErrorRecord) error {
    _, err := s.db.ExecContext(ctx, `
        INSERT INTO errors (ts, category, path, message) VALUES (?, ?, ?, ?)
    `, e.TS, e.Category, e.Path, e.Message)
    return err
}

func (s *Store) ErrorsSince(ctx context.Context, since time.Time) ([]ErrorRecord, error) {
    rows, err := s.db.QueryContext(ctx, `
        SELECT id, ts, category, COALESCE(path, ''), message
        FROM errors WHERE ts >= ? ORDER BY ts`, since)
    if err != nil {
        return nil, err
    }
    defer rows.Close()
    var out []ErrorRecord
    for rows.Next() {
        var e ErrorRecord
        if err := rows.Scan(&e.ID, &e.TS, &e.Category, &e.Path, &e.Message); err != nil {
            return nil, err
        }
        out = append(out, e)
    }
    return out, rows.Err()
}
```

- [ ] **Step 4: Run — must pass**

```bash
go test ./internal/store/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/store/
git commit -m "feat(store): add accessors for runs, clean_events, errors"
```

---

### Task 11: Add aggregation queries for stats

**Files:**
- Create: `internal/store/stats.go`
- Create: `internal/store/stats_test.go`

- [ ] **Step 1: Failing test**

`internal/store/stats_test.go`:
```go
package store

import (
    "context"
    "testing"
    "time"
)

func TestTotalBytesRecovered(t *testing.T) {
    s := newTestStore(t)
    ctx := context.Background()
    runID, _ := s.StartRun(ctx, time.Now())
    for _, b := range []int64{1000, 500, 200} {
        _ = s.RecordCleanEvent(ctx, CleanEvent{
            RunID: runID, TS: time.Now(), Path: "/p",
            BytesBefore: b, BytesAfter: 0,
        })
    }
    total, err := s.TotalBytesRecovered(ctx, time.Time{})
    if err != nil {
        t.Fatalf("TotalBytesRecovered: %v", err)
    }
    if total != 1700 {
        t.Fatalf("total = %d, want 1700", total)
    }
}

func TestTopProjectsByBytes(t *testing.T) {
    s := newTestStore(t)
    ctx := context.Background()
    runID, _ := s.StartRun(ctx, time.Now())
    paths := map[string]int64{"/a": 10, "/b": 50, "/c": 30}
    for p, b := range paths {
        _ = s.RecordCleanEvent(ctx, CleanEvent{
            RunID: runID, TS: time.Now(), Path: p,
            BytesBefore: b, BytesAfter: 0,
        })
    }
    top, err := s.TopProjectsByBytes(ctx, time.Time{}, 2)
    if err != nil {
        t.Fatalf("TopProjectsByBytes: %v", err)
    }
    if len(top) != 2 || top[0].Path != "/b" || top[1].Path != "/c" {
        t.Fatalf("top = %+v", top)
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./internal/store/...
```

Expected: FAIL.

- [ ] **Step 3: Implement**

`internal/store/stats.go`:
```go
package store

import (
    "context"
    "time"
)

type ProjectBytes struct {
    Path  string
    Bytes int64
}

func (s *Store) TotalBytesRecovered(ctx context.Context, since time.Time) (int64, error) {
    var total int64
    err := s.db.QueryRowContext(ctx, `
        SELECT COALESCE(SUM(bytes_before - bytes_after), 0)
        FROM clean_events WHERE ts >= ?`, since).Scan(&total)
    return total, err
}

func (s *Store) TopProjectsByBytes(ctx context.Context, since time.Time, n int) ([]ProjectBytes, error) {
    rows, err := s.db.QueryContext(ctx, `
        SELECT path, SUM(bytes_before - bytes_after) AS recovered
        FROM clean_events WHERE ts >= ?
        GROUP BY path
        ORDER BY recovered DESC
        LIMIT ?`, since, n)
    if err != nil {
        return nil, err
    }
    defer rows.Close()
    var out []ProjectBytes
    for rows.Next() {
        var p ProjectBytes
        if err := rows.Scan(&p.Path, &p.Bytes); err != nil {
            return nil, err
        }
        out = append(out, p)
    }
    return out, rows.Err()
}
```

- [ ] **Step 4: Run — must pass**

```bash
go test ./internal/store/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/store/
git commit -m "feat(store): add aggregation queries for stats"
```

---

### Task 12: Add `cache` package wrapping the projects table with `Verify`

**Files:**
- Create: `internal/cache/cache.go`
- Create: `internal/cache/cache_test.go`

- [ ] **Step 1: Failing test for `Verify`**

`internal/cache/cache_test.go`:
```go
package cache

import (
    "context"
    "os"
    "path/filepath"
    "testing"
    "time"

    "github.com/dcchuck/car-go-clean/internal/store"
)

func newCacheT(t *testing.T) (*Cache, *store.Store) {
    t.Helper()
    s, err := store.Open(context.Background(), filepath.Join(t.TempDir(), "s.db"))
    if err != nil {
        t.Fatal(err)
    }
    if err := s.Migrate(context.Background()); err != nil {
        t.Fatal(err)
    }
    t.Cleanup(func() { s.Close() })
    return New(s), s
}

func TestVerify_TrueWhenCargoTomlExists(t *testing.T) {
    dir := t.TempDir()
    if err := os.WriteFile(filepath.Join(dir, "Cargo.toml"), []byte("[package]\nname=\"x\"\nversion=\"0.1.0\"\n"), 0o644); err != nil {
        t.Fatal(err)
    }
    c, _ := newCacheT(t)
    ok, err := c.Verify(dir)
    if err != nil {
        t.Fatalf("Verify: %v", err)
    }
    if !ok {
        t.Fatalf("Verify(%s) = false, want true", dir)
    }
}

func TestVerify_FalseWhenDirMissing(t *testing.T) {
    c, _ := newCacheT(t)
    ok, err := c.Verify("/nonexistent")
    if err != nil {
        t.Fatalf("Verify: %v", err)
    }
    if ok {
        t.Fatal("Verify on missing dir should return false")
    }
}

func TestSyncRemovesDeadEntries(t *testing.T) {
    dir := t.TempDir()
    _ = os.WriteFile(filepath.Join(dir, "Cargo.toml"), []byte("[package]\nname=\"x\"\nversion=\"0.1.0\"\n"), 0o644)
    c, s := newCacheT(t)
    ctx := context.Background()
    _ = s.UpsertProject(ctx, dir, time.Now())
    _ = s.UpsertProject(ctx, "/definitely/not/here", time.Now())

    removed, err := c.SyncOnDisk(ctx)
    if err != nil {
        t.Fatalf("SyncOnDisk: %v", err)
    }
    if len(removed) != 1 || removed[0] != "/definitely/not/here" {
        t.Fatalf("removed = %v", removed)
    }
    remaining, _ := s.AllProjects(ctx)
    if len(remaining) != 1 || remaining[0].Path != dir {
        t.Fatalf("remaining = %v", remaining)
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./internal/cache/...
```

Expected: FAIL.

- [ ] **Step 3: Implement**

`internal/cache/cache.go`:
```go
// Package cache wraps the projects table with verification and cleanup
// helpers.
package cache

import (
    "context"
    "os"
    "path/filepath"

    "github.com/dcchuck/car-go-clean/internal/store"
)

type Cache struct {
    s *store.Store
}

func New(s *store.Store) *Cache { return &Cache{s: s} }

// Verify returns true when the path still exists on disk and still contains
// a Cargo.toml. It does not parse the Cargo.toml.
func (c *Cache) Verify(path string) (bool, error) {
    info, err := os.Stat(path)
    if err != nil {
        if os.IsNotExist(err) {
            return false, nil
        }
        return false, err
    }
    if !info.IsDir() {
        return false, nil
    }
    _, err = os.Stat(filepath.Join(path, "Cargo.toml"))
    if err != nil {
        if os.IsNotExist(err) {
            return false, nil
        }
        return false, err
    }
    return true, nil
}

// SyncOnDisk verifies every cached project and removes entries whose paths
// no longer look like Rust projects. Returns the list of removed paths.
func (c *Cache) SyncOnDisk(ctx context.Context) ([]string, error) {
    projects, err := c.s.AllProjects(ctx)
    if err != nil {
        return nil, err
    }
    var removed []string
    for _, p := range projects {
        ok, err := c.Verify(p.Path)
        if err != nil {
            return removed, err
        }
        if !ok {
            if err := c.s.RemoveProject(ctx, p.Path); err != nil {
                return removed, err
            }
            removed = append(removed, p.Path)
        }
    }
    return removed, nil
}
```

- [ ] **Step 4: Run — must pass**

```bash
go test ./internal/cache/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/cache/
git commit -m "feat(cache): add Cache.Verify and Cache.SyncOnDisk"
```

---

## Chunk 3: Scanner and Cleaner Packages

This chunk produces the discovery (`scanner`) and execution (`cleaner`) units. Both are constructed with injectable dependencies so they can be tested without touching the real filesystem or invoking real `cargo`.

**Note on spec deviation:** The spec sketches the scanner as returning "a stream of project roots (channel)". This plan returns `[]string` instead. Rationale: workloads are bounded by a user's filesystem, the cache write happens once per cycle (not incrementally per project), and a slice is simpler to test. Revisit if scan latency becomes user-visible.

### Task 13: Scanner with `FS` interface

**Files:**
- Create: `internal/scanner/fs.go`
- Create: `internal/scanner/scanner.go`
- Create: `internal/scanner/scanner_test.go`

- [ ] **Step 1: Define the `FS` interface and the real implementation**

`internal/scanner/fs.go`:
```go
package scanner

import (
    "io/fs"
    "os"
    "path/filepath"
)

// FS is the minimal surface the scanner needs from a filesystem. It is
// intentionally small so tests can supply in-memory trees.
type FS interface {
    // WalkDir invokes fn for each file/dir under root, like filepath.WalkDir.
    WalkDir(root string, fn fs.WalkDirFunc) error
    // Stat returns os.Stat-equivalent info.
    Stat(path string) (fs.FileInfo, error)
}

type realFS struct{}

func (realFS) WalkDir(root string, fn fs.WalkDirFunc) error { return filepath.WalkDir(root, fn) }
func (realFS) Stat(p string) (fs.FileInfo, error)            { return os.Stat(p) }

// RealFS is the default filesystem implementation.
func RealFS() FS { return realFS{} }
```

- [ ] **Step 2: Failing test using a temp dir tree**

`internal/scanner/scanner_test.go`:
```go
package scanner

import (
    "context"
    "os"
    "path/filepath"
    "sort"
    "testing"
)

func writeFile(t *testing.T, path, body string) {
    t.Helper()
    if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
        t.Fatal(err)
    }
    if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
        t.Fatal(err)
    }
}

func TestScan_FindsCargoTomlAndStopsDescending(t *testing.T) {
    root := t.TempDir()
    writeFile(t, filepath.Join(root, "proj-a", "Cargo.toml"), "[package]\nname=\"a\"\nversion=\"0.1.0\"\n")
    writeFile(t, filepath.Join(root, "proj-a", "sub", "Cargo.toml"), "[package]\nname=\"a-sub\"\nversion=\"0.1.0\"\n")
    writeFile(t, filepath.Join(root, "deep", "x", "y", "Cargo.toml"), "[package]\nname=\"y\"\nversion=\"0.1.0\"\n")
    writeFile(t, filepath.Join(root, "ignore-me", "node_modules", "Cargo.toml"), "")

    sc := New(Options{
        FS:       RealFS(),
        Roots:    []string{root},
        Excludes: []string{"node_modules"},
    })
    got, err := sc.Scan(context.Background())
    if err != nil {
        t.Fatalf("Scan: %v", err)
    }
    sort.Strings(got)
    want := []string{
        filepath.Join(root, "deep", "x", "y"),
        filepath.Join(root, "proj-a"),
    }
    if len(got) != len(want) || got[0] != want[0] || got[1] != want[1] {
        t.Fatalf("got %v, want %v", got, want)
    }
}

func TestScan_ProjectDirsAddedDirectly(t *testing.T) {
    root := t.TempDir()
    writeFile(t, filepath.Join(root, "Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.1.0\"\n")
    sc := New(Options{
        FS:           RealFS(),
        Roots:        nil,
        ProjectDirs:  []string{root},
    })
    got, err := sc.Scan(context.Background())
    if err != nil {
        t.Fatal(err)
    }
    if len(got) != 1 || got[0] != root {
        t.Fatalf("got %v", got)
    }
}
```

- [ ] **Step 3: Run — must fail**

```bash
go test ./internal/scanner/...
```

Expected: FAIL.

- [ ] **Step 4: Implement the scanner**

`internal/scanner/scanner.go`:
```go
// Package scanner discovers Rust projects under configured roots.
package scanner

import (
    "context"
    "io/fs"
    "path/filepath"
    "strings"
)

type Options struct {
    FS          FS
    Roots       []string
    ProjectDirs []string
    Excludes    []string
}

type Scanner struct {
    opts Options
}

func New(opts Options) *Scanner {
    if opts.FS == nil {
        opts.FS = RealFS()
    }
    return &Scanner{opts: opts}
}

// Scan walks every Roots entry and records each directory that directly
// contains a Cargo.toml, never descending into a project once found. Paths
// listed in ProjectDirs are returned as-is if they exist and contain a
// Cargo.toml.
func (s *Scanner) Scan(ctx context.Context) ([]string, error) {
    found := map[string]struct{}{}

    for _, root := range s.opts.Roots {
        err := s.opts.FS.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
            if err != nil {
                if d != nil && d.IsDir() {
                    return fs.SkipDir
                }
                return nil
            }
            if ctx.Err() != nil {
                return ctx.Err()
            }
            if !d.IsDir() {
                return nil
            }
            if s.shouldSkip(path) {
                return fs.SkipDir
            }
            if hasCargoToml(s.opts.FS, path) {
                found[path] = struct{}{}
                return fs.SkipDir // do not descend into project subtree
            }
            return nil
        })
        if err != nil && err != context.Canceled {
            return nil, err
        }
    }

    for _, p := range s.opts.ProjectDirs {
        if hasCargoToml(s.opts.FS, p) {
            found[p] = struct{}{}
        }
    }

    out := make([]string, 0, len(found))
    for k := range found {
        out = append(out, k)
    }
    return out, nil
}

func (s *Scanner) shouldSkip(path string) bool {
    base := filepath.Base(path)
    if base == "target" {
        return true
    }
    for _, ex := range s.opts.Excludes {
        if ex == "" {
            continue
        }
        if base == ex || strings.Contains(path, string(filepath.Separator)+ex+string(filepath.Separator)) {
            return true
        }
    }
    return false
}

func hasCargoToml(filesys FS, dir string) bool {
    _, err := filesys.Stat(filepath.Join(dir, "Cargo.toml"))
    return err == nil
}
```

- [ ] **Step 5: Run — must pass**

```bash
go test ./internal/scanner/...
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add internal/scanner/
git commit -m "feat(scanner): discover Rust projects, stop descending at Cargo.toml"
```

---

### Task 14: Cleaner with `CommandRunner` interface

**Files:**
- Create: `internal/cleaner/runner.go`
- Create: `internal/cleaner/cleaner.go`
- Create: `internal/cleaner/cleaner_test.go`

- [ ] **Step 1: Define the runner interface**

`internal/cleaner/runner.go`:
```go
package cleaner

import (
    "bytes"
    "context"
    "os/exec"
)

// CommandRunner is the seam between the cleaner and the real `cargo`
// binary. Tests inject a fake; production wires up RealRunner.
//
// Contract: a non-zero exit from the child process is NOT reported as
// err. err is reserved for invocation failures (binary not found,
// context cancelled before start, fork failure). Callers that want to
// detect "cargo clean failed" should look at the returned exitCode.
type CommandRunner interface {
    Run(ctx context.Context, dir, name string, args ...string) (stdout, stderr []byte, exitCode int, err error)
}

type realRunner struct{}

func (realRunner) Run(ctx context.Context, dir, name string, args ...string) ([]byte, []byte, int, error) {
    var stdout, stderr bytes.Buffer
    cmd := exec.CommandContext(ctx, name, args...)
    cmd.Dir = dir
    cmd.Stdout = &stdout
    cmd.Stderr = &stderr
    err := cmd.Run()
    if err == nil {
        return stdout.Bytes(), stderr.Bytes(), 0, nil
    }
    if ee, ok := err.(*exec.ExitError); ok {
        return stdout.Bytes(), stderr.Bytes(), ee.ExitCode(), nil
    }
    return stdout.Bytes(), stderr.Bytes(), 0, err
}

func RealRunner() CommandRunner { return realRunner{} }
```

- [ ] **Step 2: Failing test for `Clean`**

`internal/cleaner/cleaner_test.go`:
```go
package cleaner

import (
    "context"
    "os"
    "path/filepath"
    "testing"
)

type fakeRunner struct {
    calls       []struct{ dir, name string; args []string }
    stderr      []byte
    exitCode    int
    deleteTarget bool
}

func (f *fakeRunner) Run(_ context.Context, dir, name string, args ...string) ([]byte, []byte, int, error) {
    f.calls = append(f.calls, struct{ dir, name string; args []string }{dir, name, args})
    if f.deleteTarget {
        _ = os.RemoveAll(filepath.Join(dir, "target"))
    }
    return nil, f.stderr, f.exitCode, nil
}

func writeBytes(t *testing.T, path string, bytes []byte) {
    t.Helper()
    if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
        t.Fatal(err)
    }
    if err := os.WriteFile(path, bytes, 0o644); err != nil {
        t.Fatal(err)
    }
}

func TestClean_MeasuresBytesAndInvokesCargo(t *testing.T) {
    dir := t.TempDir()
    writeBytes(t, filepath.Join(dir, "Cargo.toml"), []byte("[package]\nname=\"x\"\nversion=\"0.1.0\"\n"))
    writeBytes(t, filepath.Join(dir, "target", "debug", "blob.bin"), make([]byte, 4096))

    runner := &fakeRunner{deleteTarget: true}
    c := New(Options{Runner: runner, CargoBin: "cargo"})

    res, err := c.Clean(context.Background(), dir)
    if err != nil {
        t.Fatalf("Clean: %v", err)
    }
    if res.BytesBefore < 4096 {
        t.Errorf("BytesBefore = %d, want >= 4096", res.BytesBefore)
    }
    if res.BytesAfter != 0 {
        t.Errorf("BytesAfter = %d, want 0", res.BytesAfter)
    }
    if res.ExitCode != 0 {
        t.Errorf("ExitCode = %d", res.ExitCode)
    }
    if len(runner.calls) != 1 || runner.calls[0].name != "cargo" {
        t.Errorf("call: %+v", runner.calls)
    }
}

func TestClean_SkipsWhenNoTargetDir(t *testing.T) {
    dir := t.TempDir()
    writeBytes(t, filepath.Join(dir, "Cargo.toml"), []byte("[package]\nname=\"x\"\nversion=\"0.1.0\"\n"))

    runner := &fakeRunner{}
    c := New(Options{Runner: runner, CargoBin: "cargo"})

    res, err := c.Clean(context.Background(), dir)
    if err != nil {
        t.Fatalf("Clean: %v", err)
    }
    if !res.Skipped {
        t.Errorf("want Skipped = true, got %+v", res)
    }
    if len(runner.calls) != 0 {
        t.Errorf("cargo invoked unnecessarily: %+v", runner.calls)
    }
}

func TestClean_CapturesNonZeroExit(t *testing.T) {
    dir := t.TempDir()
    writeBytes(t, filepath.Join(dir, "Cargo.toml"), []byte("[package]\nname=\"x\"\nversion=\"0.1.0\"\n"))
    writeBytes(t, filepath.Join(dir, "target", "x.bin"), []byte("x"))

    runner := &fakeRunner{exitCode: 101, stderr: []byte("error: something")}
    c := New(Options{Runner: runner, CargoBin: "cargo"})

    res, err := c.Clean(context.Background(), dir)
    if err != nil {
        t.Fatalf("Clean: %v", err)
    }
    if res.ExitCode != 101 {
        t.Errorf("ExitCode = %d, want 101", res.ExitCode)
    }
    if res.StderrExcerpt == "" {
        t.Errorf("StderrExcerpt empty, want excerpt")
    }
}
```

- [ ] **Step 3: Run — must fail**

```bash
go test ./internal/cleaner/...
```

Expected: FAIL.

- [ ] **Step 4: Implement the cleaner**

`internal/cleaner/cleaner.go`:
```go
// Package cleaner runs `cargo clean` in a single project directory and
// measures reclaimed disk space.
package cleaner

import (
    "context"
    "errors"
    "io/fs"
    "os"
    "path/filepath"
    "time"
)

const maxStderrExcerpt = 4096

type Options struct {
    Runner   CommandRunner
    CargoBin string        // typically "cargo" or an absolute path
    Timeout  time.Duration // 0 = no timeout (caller controls via ctx)
}

type Cleaner struct {
    opts Options
}

type Result struct {
    Path          string
    BytesBefore   int64
    BytesAfter    int64
    Duration      time.Duration
    ExitCode      int
    StderrExcerpt string
    Skipped       bool // true when there was no target/ to clean
}

func New(opts Options) *Cleaner {
    if opts.Runner == nil {
        opts.Runner = RealRunner()
    }
    if opts.CargoBin == "" {
        opts.CargoBin = "cargo"
    }
    return &Cleaner{opts: opts}
}

func (c *Cleaner) Clean(ctx context.Context, dir string) (Result, error) {
    res := Result{Path: dir}

    targetDir := filepath.Join(dir, "target")
    info, err := os.Stat(targetDir)
    if err != nil {
        if errors.Is(err, fs.ErrNotExist) {
            res.Skipped = true
            return res, nil
        }
        return res, err
    }
    if !info.IsDir() {
        res.Skipped = true
        return res, nil
    }

    before, err := dirSize(targetDir)
    if err != nil {
        return res, err
    }
    res.BytesBefore = before

    runCtx := ctx
    if c.opts.Timeout > 0 {
        var cancel context.CancelFunc
        runCtx, cancel = context.WithTimeout(ctx, c.opts.Timeout)
        defer cancel()
    }

    start := time.Now()
    _, stderr, code, runErr := c.opts.Runner.Run(runCtx, dir, c.opts.CargoBin, "clean")
    res.Duration = time.Since(start)
    res.ExitCode = code
    if runErr != nil {
        res.StderrExcerpt = runErr.Error()
        return res, runErr
    }
    if len(stderr) > 0 {
        if len(stderr) > maxStderrExcerpt {
            stderr = stderr[len(stderr)-maxStderrExcerpt:]
        }
        res.StderrExcerpt = string(stderr)
    }

    after, err := dirSize(targetDir)
    if err != nil {
        return res, err
    }
    res.BytesAfter = after
    return res, nil
}

func dirSize(root string) (int64, error) {
    var total int64
    err := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
        if err != nil {
            if errors.Is(err, fs.ErrNotExist) {
                return nil
            }
            return err
        }
        if d.IsDir() {
            return nil
        }
        info, err := d.Info()
        if err != nil {
            if errors.Is(err, fs.ErrNotExist) {
                return nil
            }
            return err
        }
        total += info.Size()
        return nil
    })
    return total, err
}
```

- [ ] **Step 5: Run — must pass**

```bash
go test ./internal/cleaner/...
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add internal/cleaner/
git commit -m "feat(cleaner): run cargo clean, measure reclaimed bytes"
```

---

### Task 15: `ResolveCargoBin` helper

**Files:**
- Modify: `internal/cleaner/runner.go`
- Create: `internal/cleaner/resolve_test.go`

The daemon runs under launchd/systemd which may have a stripped PATH. Resolve `cargo` once at startup.

- [ ] **Step 1: Failing test**

`internal/cleaner/resolve_test.go`:
```go
package cleaner

import (
    "os"
    "path/filepath"
    "testing"
)

func TestResolveCargoBin_PrefersAbsolutePath(t *testing.T) {
    dir := t.TempDir()
    fake := filepath.Join(dir, "cargo")
    if err := os.WriteFile(fake, []byte("#!/bin/sh\n"), 0o755); err != nil {
        t.Fatal(err)
    }
    got, err := ResolveCargoBin([]string{fake})
    if err != nil {
        t.Fatalf("ResolveCargoBin: %v", err)
    }
    if got != fake {
        t.Fatalf("got %q, want %q", got, fake)
    }
}

func TestResolveCargoBin_ErrorsWhenAllMissing(t *testing.T) {
    if _, err := ResolveCargoBin([]string{"/nope/cargo", "/also/nope/cargo"}); err == nil {
        t.Fatal("expected error")
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./internal/cleaner/...
```

Expected: FAIL.

- [ ] **Step 3: Implement**

Add the following to `internal/cleaner/runner.go`. **Replace the existing `import` block** with the merged block shown below, then append the two new functions at the bottom of the file:

```go
import (
    "bytes"
    "context"
    "errors"
    "os"
    "os/exec"
)
```

```go
// ResolveCargoBin returns the first path in candidates that exists and is
// executable. If candidates is empty, it falls back to `exec.LookPath("cargo")`.
func ResolveCargoBin(candidates []string) (string, error) {
    for _, p := range candidates {
        if info, err := os.Stat(p); err == nil && !info.IsDir() && info.Mode()&0o111 != 0 {
            return p, nil
        }
    }
    if p, err := exec.LookPath("cargo"); err == nil {
        return p, nil
    }
    return "", errors.New("cargo not found in any candidate path or $PATH")
}

// DefaultCargoCandidates returns common locations to probe for `cargo`.
func DefaultCargoCandidates() []string {
    home, _ := os.UserHomeDir()
    return []string{
        home + "/.cargo/bin/cargo",
        "/opt/homebrew/bin/cargo",
        "/usr/local/bin/cargo",
    }
}
```

- [ ] **Step 4: Run — must pass**

```bash
go test ./internal/cleaner/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/cleaner/
git commit -m "feat(cleaner): add ResolveCargoBin helper for launchd PATH"
```

---

## Chunk 4: Daemon, Logging, and Lock

This chunk wires components into the long-running daemon: a scheduler driven by a `Clock` interface, advisory locking via flock, and structured logging.

### Task 16: `Clock` interface and `RealClock`

**Files:**
- Create: `internal/daemon/clock.go`
- Create: `internal/daemon/clock_test.go`

- [ ] **Step 1: Failing test for `FakeClock`**

`internal/daemon/clock_test.go`:
```go
package daemon

import (
    "testing"
    "time"
)

func TestFakeClock_NowAdvances(t *testing.T) {
    fc := NewFakeClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))
    if got := fc.Now(); !got.Equal(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)) {
        t.Fatalf("Now = %v", got)
    }
    fc.Advance(2 * time.Hour)
    if got := fc.Now(); !got.Equal(time.Date(2026, 1, 1, 2, 0, 0, 0, time.UTC)) {
        t.Fatalf("Now after advance = %v", got)
    }
}

func TestFakeClock_TickerFiresOnAdvance(t *testing.T) {
    fc := NewFakeClock(time.Now())
    ch := fc.NewTicker(time.Hour).C()
    fc.Advance(2 * time.Hour)
    fires := 0
    for {
        select {
        case <-ch:
            fires++
        default:
            if fires < 2 {
                t.Fatalf("expected 2 fires, got %d", fires)
            }
            return
        }
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./internal/daemon/...
```

Expected: FAIL.

- [ ] **Step 3: Implement**

`internal/daemon/clock.go`:
```go
package daemon

import (
    "sync"
    "time"
)

type Clock interface {
    Now() time.Time
    NewTicker(d time.Duration) Ticker
}

type Ticker interface {
    C() <-chan time.Time
    Stop()
}

type realClock struct{}

func RealClock() Clock { return realClock{} }
func (realClock) Now() time.Time { return time.Now() }
func (realClock) NewTicker(d time.Duration) Ticker {
    t := time.NewTicker(d)
    return realTicker{t}
}

type realTicker struct{ t *time.Ticker }
func (r realTicker) C() <-chan time.Time { return r.t.C }
func (r realTicker) Stop()               { r.t.Stop() }

type FakeClock struct {
    mu      sync.Mutex
    now     time.Time
    tickers []*fakeTicker
}

func NewFakeClock(start time.Time) *FakeClock { return &FakeClock{now: start} }

func (f *FakeClock) Now() time.Time {
    f.mu.Lock()
    defer f.mu.Unlock()
    return f.now
}

func (f *FakeClock) Advance(d time.Duration) {
    f.mu.Lock()
    f.now = f.now.Add(d)
    snapshot := append([]*fakeTicker(nil), f.tickers...)
    now := f.now
    f.mu.Unlock()
    for _, t := range snapshot {
        for !t.next.After(now) {
            select {
            case t.ch <- t.next:
            default:
            }
            t.next = t.next.Add(t.interval)
        }
    }
}

func (f *FakeClock) NewTicker(d time.Duration) Ticker {
    f.mu.Lock()
    defer f.mu.Unlock()
    t := &fakeTicker{
        ch:       make(chan time.Time, 16),
        interval: d,
        next:     f.now.Add(d),
    }
    f.tickers = append(f.tickers, t)
    return t
}

type fakeTicker struct {
    ch       chan time.Time
    interval time.Duration
    next     time.Time
}

func (t *fakeTicker) C() <-chan time.Time { return t.ch }
func (t *fakeTicker) Stop()               {}
```

- [ ] **Step 4: Run — must pass**

```bash
go test ./internal/daemon/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/daemon/
git commit -m "feat(daemon): add Clock interface with RealClock and FakeClock"
```

---

### Task 17: `Lock` package using flock

**Files:**
- Create: `internal/lockfile/lockfile.go`
- Create: `internal/lockfile/lockfile_test.go`

- [ ] **Step 1: Add the sys dependency**

```bash
go get golang.org/x/sys/unix@latest
go mod tidy
```

- [ ] **Step 2: Failing test for `TryAcquire`**

`internal/lockfile/lockfile_test.go`:
```go
package lockfile

import (
    "path/filepath"
    "testing"
)

func TestTryAcquire_SecondAcquireFails(t *testing.T) {
    path := filepath.Join(t.TempDir(), "x.lock")

    a, err := TryAcquire(path)
    if err != nil {
        t.Fatalf("first TryAcquire: %v", err)
    }
    defer a.Release()

    b, err := TryAcquire(path)
    if err == nil {
        b.Release()
        t.Fatal("expected second TryAcquire to fail")
    }
}

func TestRelease_AllowsReAcquire(t *testing.T) {
    path := filepath.Join(t.TempDir(), "x.lock")
    a, err := TryAcquire(path)
    if err != nil {
        t.Fatalf("first: %v", err)
    }
    a.Release()
    b, err := TryAcquire(path)
    if err != nil {
        t.Fatalf("re-acquire: %v", err)
    }
    b.Release()
}
```

- [ ] **Step 3: Run — must fail**

```bash
go test ./internal/lockfile/...
```

Expected: FAIL.

- [ ] **Step 4: Implement**

`internal/lockfile/lockfile.go`:
```go
// Package lockfile provides an advisory file lock used to ensure only one
// process at a time mutates the car-go-clean state.
package lockfile

import (
    "fmt"
    "os"
    "path/filepath"

    "golang.org/x/sys/unix"
)

type Lock struct {
    f *os.File
}

func TryAcquire(path string) (*Lock, error) {
    if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
        return nil, err
    }
    f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o644)
    if err != nil {
        return nil, err
    }
    if err := unix.Flock(int(f.Fd()), unix.LOCK_EX|unix.LOCK_NB); err != nil {
        f.Close()
        return nil, fmt.Errorf("acquire lock %s: %w", path, err)
    }
    return &Lock{f: f}, nil
}

func (l *Lock) Release() error {
    if l == nil || l.f == nil {
        return nil
    }
    _ = unix.Flock(int(l.f.Fd()), unix.LOCK_UN)
    err := l.f.Close()
    l.f = nil
    return err
}
```

- [ ] **Step 5: Run — must pass**

```bash
go test ./internal/lockfile/...
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add go.mod go.sum internal/lockfile/
git commit -m "feat(lockfile): add flock-based advisory lock"
```

---

### Task 18: `logging` package wrapping slog + lumberjack

**Files:**
- Create: `internal/logging/logging.go`
- Create: `internal/logging/logging_test.go`

- [ ] **Step 1: Add lumberjack**

```bash
go get gopkg.in/natefinch/lumberjack.v2@latest
go mod tidy
```

- [ ] **Step 2: Failing test**

`internal/logging/logging_test.go`:
```go
package logging

import (
    "os"
    "path/filepath"
    "testing"
)

func TestNew_WritesToFile(t *testing.T) {
    dir := t.TempDir()
    path := filepath.Join(dir, "x.log")
    l, err := New(Options{LogPath: path, Level: "info"})
    if err != nil {
        t.Fatalf("New: %v", err)
    }
    l.Logger.Info("hello", "k", "v")
    if err := l.Close(); err != nil {
        t.Fatalf("Close: %v", err)
    }
    b, err := os.ReadFile(path)
    if err != nil {
        t.Fatalf("ReadFile: %v", err)
    }
    if len(b) == 0 {
        t.Fatal("log file empty")
    }
}

func TestNew_RejectsUnknownLevel(t *testing.T) {
    if _, err := New(Options{LogPath: "/tmp/x.log", Level: "verbose"}); err == nil {
        t.Fatal("expected error")
    }
}
```

- [ ] **Step 3: Run — must fail**

```bash
go test ./internal/logging/...
```

Expected: FAIL.

- [ ] **Step 4: Implement**

`internal/logging/logging.go`:
```go
// Package logging configures slog with both stderr and a rotating file sink.
package logging

import (
    "fmt"
    "io"
    "log/slog"
    "os"
    "path/filepath"
    "strings"

    "gopkg.in/natefinch/lumberjack.v2"
)

type Options struct {
    LogPath string
    Level   string
}

type Logger struct {
    *slog.Logger
    closer io.Closer
}

func New(opts Options) (*Logger, error) {
    lvl, err := parseLevel(opts.Level)
    if err != nil {
        return nil, err
    }
    if err := os.MkdirAll(filepath.Dir(opts.LogPath), 0o755); err != nil {
        return nil, err
    }
    rot := &lumberjack.Logger{
        Filename:   opts.LogPath,
        MaxSize:    20, // MB
        MaxBackups: 3,
        MaxAge:     30,
        Compress:   true,
    }
    multi := io.MultiWriter(os.Stderr, rot)
    h := slog.NewJSONHandler(multi, &slog.HandlerOptions{Level: lvl})
    return &Logger{Logger: slog.New(h), closer: rot}, nil
}

func (l *Logger) Close() error {
    if l.closer != nil {
        return l.closer.Close()
    }
    return nil
}

func parseLevel(s string) (slog.Level, error) {
    switch strings.ToLower(s) {
    case "debug":
        return slog.LevelDebug, nil
    case "", "info":
        return slog.LevelInfo, nil
    case "warn":
        return slog.LevelWarn, nil
    case "error":
        return slog.LevelError, nil
    default:
        return 0, fmt.Errorf("unknown log level %q", s)
    }
}
```

- [ ] **Step 5: Run — must pass**

```bash
go test ./internal/logging/...
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add go.mod go.sum internal/logging/
git commit -m "feat(logging): add slog + lumberjack logger"
```

---

### Task 19: Daemon main loop with `RunCycle` and `ScanCycle`

**Files:**
- Create: `internal/daemon/daemon.go`
- Create: `internal/daemon/daemon_test.go`

- [ ] **Step 1: Failing test wiring fakes through a clean cycle**

`internal/daemon/daemon_test.go`:
```go
package daemon

import (
    "context"
    "log/slog"
    "os"
    "path/filepath"
    "testing"
    "time"

    "github.com/dcchuck/car-go-clean/internal/cache"
    "github.com/dcchuck/car-go-clean/internal/cleaner"
    "github.com/dcchuck/car-go-clean/internal/scanner"
    "github.com/dcchuck/car-go-clean/internal/store"
)

type fakeRunner struct{ deleteTarget bool }

func (f fakeRunner) Run(_ context.Context, dir, _ string, _ ...string) ([]byte, []byte, int, error) {
    if f.deleteTarget {
        _ = os.RemoveAll(filepath.Join(dir, "target"))
    }
    return nil, nil, 0, nil
}

func writeFile(t *testing.T, p, body string) {
    t.Helper()
    if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
        t.Fatal(err)
    }
    if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
        t.Fatal(err)
    }
}

func TestRunCycle_CleansCachedProjectsAndRecordsRun(t *testing.T) {
    root := t.TempDir()
    proj := filepath.Join(root, "proj")
    writeFile(t, filepath.Join(proj, "Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.1.0\"\n")
    writeFile(t, filepath.Join(proj, "target", "blob.bin"), string(make([]byte, 2048)))

    ctx := context.Background()
    s, err := store.Open(ctx, filepath.Join(t.TempDir(), "s.db"))
    if err != nil {
        t.Fatal(err)
    }
    defer s.Close()
    if err := s.Migrate(ctx); err != nil {
        t.Fatal(err)
    }
    _ = s.UpsertProject(ctx, proj, time.Now())

    d := New(Deps{
        Store:    s,
        Cache:    cache.New(s),
        Scanner:  scanner.New(scanner.Options{Roots: []string{root}}),
        Cleaner:  cleaner.New(cleaner.Options{Runner: fakeRunner{deleteTarget: true}, CargoBin: "cargo"}),
        Clock:    RealClock(),
        Logger:   slog.Default(),
    })

    err = d.RunCycle(ctx)
    if err != nil {
        t.Fatalf("RunCycle: %v", err)
    }

    r, err := s.LastRun(ctx)
    if err != nil {
        t.Fatalf("LastRun: %v", err)
    }
    if r.ProjectsCleaned != 1 || r.BytesRecovered < 2048 {
        t.Fatalf("run = %+v", r)
    }
}

func TestRunCycle_RemovesDeadCachedProjects(t *testing.T) {
    ctx := context.Background()
    s, _ := store.Open(ctx, filepath.Join(t.TempDir(), "s.db"))
    defer s.Close()
    _ = s.Migrate(ctx)
    _ = s.UpsertProject(ctx, "/definitely/missing", time.Now())

    d := New(Deps{
        Store:    s,
        Cache:    cache.New(s),
        Scanner:  scanner.New(scanner.Options{}),
        Cleaner:  cleaner.New(cleaner.Options{Runner: fakeRunner{}, CargoBin: "cargo"}),
        Clock:    RealClock(),
        Logger:   slog.Default(),
    })

    if err := d.RunCycle(ctx); err != nil {
        t.Fatalf("RunCycle: %v", err)
    }
    ps, _ := s.AllProjects(ctx)
    if len(ps) != 0 {
        t.Fatalf("dead path not removed: %+v", ps)
    }
}

func TestScanCycle_PopulatesCache(t *testing.T) {
    root := t.TempDir()
    writeFile(t, filepath.Join(root, "a", "Cargo.toml"), "[package]\nname=\"a\"\nversion=\"0.1.0\"\n")
    writeFile(t, filepath.Join(root, "b", "Cargo.toml"), "[package]\nname=\"b\"\nversion=\"0.1.0\"\n")

    ctx := context.Background()
    s, _ := store.Open(ctx, filepath.Join(t.TempDir(), "s.db"))
    defer s.Close()
    _ = s.Migrate(ctx)

    d := New(Deps{
        Store:   s,
        Cache:   cache.New(s),
        Scanner: scanner.New(scanner.Options{Roots: []string{root}}),
        Cleaner: cleaner.New(cleaner.Options{Runner: fakeRunner{}, CargoBin: "cargo"}),
        Clock:   RealClock(),
        Logger:  slog.Default(),
    })
    if err := d.ScanCycle(ctx); err != nil {
        t.Fatalf("ScanCycle: %v", err)
    }
    ps, _ := s.AllProjects(ctx)
    if len(ps) != 2 {
        t.Fatalf("got %d projects, want 2", len(ps))
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./internal/daemon/...
```

Expected: FAIL.

- [ ] **Step 3: Implement**

`internal/daemon/daemon.go`:
```go
// Package daemon is the long-running scheduler that ties scanner, cache,
// and cleaner together.
package daemon

import (
    "context"
    "log/slog"

    "github.com/dcchuck/car-go-clean/internal/cache"
    "github.com/dcchuck/car-go-clean/internal/cleaner"
    "github.com/dcchuck/car-go-clean/internal/scanner"
    "github.com/dcchuck/car-go-clean/internal/store"
)

type Deps struct {
    Store   *store.Store
    Cache   *cache.Cache
    Scanner *scanner.Scanner
    Cleaner *cleaner.Cleaner
    Clock   Clock
    Logger  *slog.Logger
}

type Daemon struct {
    deps Deps
}

func New(deps Deps) *Daemon { return &Daemon{deps: deps} }

// ScanCycle discovers Rust projects under the configured roots and upserts
// them into the cache. Existing entries are left in place (UpsertProject's
// ON CONFLICT keeps DiscoveredAt and updates LastSeenAt).
func (d *Daemon) ScanCycle(ctx context.Context) error {
    paths, err := d.deps.Scanner.Scan(ctx)
    if err != nil {
        _ = d.deps.Store.RecordError(ctx, store.ErrorRecord{
            TS: d.deps.Clock.Now(), Category: "scan", Message: err.Error(),
        })
        return err
    }
    now := d.deps.Clock.Now()
    for _, p := range paths {
        if err := d.deps.Store.UpsertProject(ctx, p, now); err != nil {
            d.deps.Logger.Error("upsert project", "path", p, "err", err)
        }
    }
    d.deps.Logger.Info("scan complete", "found", len(paths))
    return nil
}

// RunCycle removes dead entries from the cache, then runs cargo clean for
// every surviving entry, recording the run and per-project events.
func (d *Daemon) RunCycle(ctx context.Context) error {
    removed, err := d.deps.Cache.SyncOnDisk(ctx)
    if err != nil {
        return err
    }
    for _, p := range removed {
        d.deps.Logger.Info("removed dead project", "path", p)
    }

    runID, err := d.deps.Store.StartRun(ctx, d.deps.Clock.Now())
    if err != nil {
        return err
    }
    projects, err := d.deps.Store.AllProjects(ctx)
    if err != nil {
        return err
    }

    var (
        cleaned   int64
        recovered int64
        errCount  int64
    )
    for _, p := range projects {
        if ctx.Err() != nil {
            break
        }
        res, err := d.deps.Cleaner.Clean(ctx, p.Path)
        if err != nil {
            errCount++
            _ = d.deps.Store.RecordError(ctx, store.ErrorRecord{
                TS:       d.deps.Clock.Now(),
                Category: "clean",
                Path:     p.Path,
                Message:  err.Error(),
            })
            d.deps.Logger.Error("clean failed", "path", p.Path, "err", err)
            continue
        }
        if res.Skipped {
            continue
        }
        cleaned++
        recovered += res.BytesBefore - res.BytesAfter
        _ = d.deps.Store.RecordCleanEvent(ctx, store.CleanEvent{
            RunID:         runID,
            TS:            d.deps.Clock.Now(),
            Path:          p.Path,
            BytesBefore:   res.BytesBefore,
            BytesAfter:    res.BytesAfter,
            DurationMS:    res.Duration.Milliseconds(),
            ExitCode:      res.ExitCode,
            StderrExcerpt: res.StderrExcerpt,
        })
        _ = d.deps.Store.MarkProjectCleaned(ctx, p.Path, d.deps.Clock.Now())
    }

    if err := d.deps.Store.FinishRun(ctx, runID, d.deps.Clock.Now(), cleaned, recovered, errCount); err != nil {
        return err
    }
    d.deps.Logger.Info("run complete", "cleaned", cleaned, "recovered", recovered, "errors", errCount)
    return nil
}
```

- [ ] **Step 4: Run — must pass**

```bash
go test ./internal/daemon/...
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/daemon/
git commit -m "feat(daemon): add RunCycle and ScanCycle"
```

---

### Task 20: Long-running `Daemon.Run` driven by tickers

**Files:**
- Modify: `internal/daemon/daemon.go`
- Modify: `internal/daemon/daemon_test.go`

- [ ] **Step 1: Failing test using `FakeClock`**

Append to `internal/daemon/daemon_test.go`:
```go
func TestRun_TriggersScanOnEmptyCacheAndThenRunsCycle(t *testing.T) {
    root := t.TempDir()
    proj := filepath.Join(root, "proj")
    writeFile(t, filepath.Join(proj, "Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.1.0\"\n")
    writeFile(t, filepath.Join(proj, "target", "blob.bin"), string(make([]byte, 1024)))

    ctx, cancel := context.WithCancel(context.Background())
    defer cancel()

    s, _ := store.Open(ctx, filepath.Join(t.TempDir(), "s.db"))
    defer s.Close()
    _ = s.Migrate(ctx)

    fc := NewFakeClock(time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC))

    d := New(Deps{
        Store:   s,
        Cache:   cache.New(s),
        Scanner: scanner.New(scanner.Options{Roots: []string{root}}),
        Cleaner: cleaner.New(cleaner.Options{Runner: fakeRunner{deleteTarget: true}, CargoBin: "cargo"}),
        Clock:   fc,
        Logger:  slog.Default(),
    })

    done := make(chan error, 1)
    go func() {
        done <- d.Run(ctx, Config{CleanInterval: time.Hour, ScanInterval: 24 * time.Hour})
    }()

    // Drive the daemon: it should scan immediately because cache is empty,
    // then a tick should fire a clean cycle one hour in.
    deadline := time.Now().Add(2 * time.Second)
    for time.Now().Before(deadline) {
        ps, _ := s.AllProjects(ctx)
        if len(ps) == 1 {
            break
        }
        time.Sleep(10 * time.Millisecond)
    }
    fc.Advance(time.Hour + time.Second)
    deadline = time.Now().Add(2 * time.Second)
    for time.Now().Before(deadline) {
        r, err := s.LastRun(ctx)
        if err == nil && r.FinishedAt != nil && r.ProjectsCleaned == 1 {
            cancel()
            <-done
            return
        }
        time.Sleep(10 * time.Millisecond)
    }
    cancel()
    <-done
    t.Fatal("expected one finished run with one project cleaned")
}
```

- [ ] **Step 2: Implement `Run`**

Add `"time"` to the existing `import (...)` block in `internal/daemon/daemon.go`, then append the following type and method at the bottom of the file:

```go
type Config struct {
    CleanInterval time.Duration
    ScanInterval  time.Duration
}

func (d *Daemon) Run(ctx context.Context, cfg Config) error {
    // Initial scan if the cache is empty.
    projects, err := d.deps.Store.AllProjects(ctx)
    if err != nil {
        return err
    }
    if len(projects) == 0 {
        if err := d.ScanCycle(ctx); err != nil {
            d.deps.Logger.Error("initial scan", "err", err)
        }
    }

    cleanT := d.deps.Clock.NewTicker(cfg.CleanInterval)
    scanT := d.deps.Clock.NewTicker(cfg.ScanInterval)
    defer cleanT.Stop()
    defer scanT.Stop()

    for {
        select {
        case <-ctx.Done():
            return ctx.Err()
        case <-cleanT.C():
            if err := d.RunCycle(ctx); err != nil {
                d.deps.Logger.Error("run cycle", "err", err)
            }
        case <-scanT.C():
            if err := d.ScanCycle(ctx); err != nil {
                d.deps.Logger.Error("scan cycle", "err", err)
            }
        }
    }
}
```

- [ ] **Step 3: Run — must pass (note: this test is timing-sensitive but bounded)**

```bash
go test ./internal/daemon/... -race -timeout 30s
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add internal/daemon/
git commit -m "feat(daemon): add long-running Run loop with tickers"
```

---

## Chunk 5: CLI Subcommands

This chunk replaces the skeleton `main.go` with a real Cobra-based CLI dispatch and adds every user-facing subcommand.

### Task 21: Add Cobra and wire `version`

**Files:**
- Modify: `cmd/car-go-clean/main.go`
- Create: `cmd/car-go-clean/root.go`
- Create: `cmd/car-go-clean/version.go`
- Modify: `go.mod`

- [ ] **Step 1: Add dependency**

```bash
go get github.com/spf13/cobra@latest
go mod tidy
```

- [ ] **Step 2: Replace `main.go` with a tiny dispatcher**

`cmd/car-go-clean/main.go`:
```go
package main

import (
    "fmt"
    "os"
)

func main() {
    if err := newRootCmd().Execute(); err != nil {
        fmt.Fprintln(os.Stderr, err)
        os.Exit(1)
    }
}
```

- [ ] **Step 3: Add root command**

`cmd/car-go-clean/root.go`:
```go
package main

import "github.com/spf13/cobra"

var version = "dev"

func newRootCmd() *cobra.Command {
    cmd := &cobra.Command{
        Use:           "car-go-clean",
        Short:         "Periodically run cargo clean on every Rust project on disk.",
        SilenceUsage:  true,
        SilenceErrors: true,
    }
    cmd.AddCommand(newVersionCmd())
    return cmd
}
```

- [ ] **Step 4: Add version subcommand**

`cmd/car-go-clean/version.go`:
```go
package main

import (
    "fmt"

    "github.com/spf13/cobra"
)

func newVersionCmd() *cobra.Command {
    return &cobra.Command{
        Use:   "version",
        Short: "Print version and exit",
        RunE: func(cmd *cobra.Command, args []string) error {
            fmt.Fprintln(cmd.OutOrStdout(), version)
            return nil
        },
    }
}
```

- [ ] **Step 5: Verify**

```bash
make build && ./bin/car-go-clean version
```

Expected: prints `dev` and exits 0.

- [ ] **Step 6: Commit**

```bash
git add go.mod go.sum cmd/car-go-clean/
git commit -m "feat(cli): switch main to Cobra dispatch with version cmd"
```

---

### Task 22: Add `health` subcommand

**Files:**
- Create: `cmd/car-go-clean/health.go`
- Create: `cmd/car-go-clean/health_test.go`

`health` checks the things a user can fix:
1. Config file parses (or no file = fine).
2. Each `scan_dir` and `project_dir` exists.
3. `cargo` is resolvable.
4. State DB is reachable and migrations apply.
5. Last 24h errors > 0 → warn (still exit 0, just print counts).

It exits non-zero only when 1–4 are broken.

- [ ] **Step 1: Failing test (table-driven, exits via cobra `RunE`)**

`cmd/car-go-clean/health_test.go`:
```go
package main

import (
    "bytes"
    "os"
    "path/filepath"
    "strings"
    "testing"
)

func TestHealth_FailsWhenScanDirMissing(t *testing.T) {
    cfgDir := t.TempDir()
    cfgPath := filepath.Join(cfgDir, "config.toml")
    _ = os.WriteFile(cfgPath, []byte(`scan_dirs = ["/definitely/missing"]`+"\n"), 0o644)

    cmd := newHealthCmd()
    var out bytes.Buffer
    cmd.SetOut(&out)
    cmd.SetErr(&out)
    cmd.SetArgs([]string{"--config", cfgPath, "--state-dir", t.TempDir()})

    err := cmd.Execute()
    if err == nil {
        t.Fatalf("expected error, got nil; output: %s", out.String())
    }
    if !strings.Contains(out.String(), "/definitely/missing") {
        t.Errorf("output missing path: %s", out.String())
    }
}

func TestHealth_PassesWithDefaults(t *testing.T) {
    cmd := newHealthCmd()
    var out bytes.Buffer
    cmd.SetOut(&out)
    cmd.SetErr(&out)
    cmd.SetArgs([]string{"--state-dir", t.TempDir(), "--skip-cargo"})

    if err := cmd.Execute(); err != nil {
        t.Fatalf("Execute: %v\noutput: %s", err, out.String())
    }
    if !strings.Contains(out.String(), "OK") {
        t.Errorf("expected OK in output, got: %s", out.String())
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
go test ./cmd/car-go-clean/...
```

Expected: FAIL.

- [ ] **Step 3: Implement**

`cmd/car-go-clean/health.go`:
```go
package main

import (
    "context"
    "fmt"
    "os"
    "path/filepath"
    "time"

    "github.com/spf13/cobra"

    "github.com/dcchuck/car-go-clean/internal/cleaner"
    "github.com/dcchuck/car-go-clean/internal/config"
    "github.com/dcchuck/car-go-clean/internal/store"
)

func newHealthCmd() *cobra.Command {
    var (
        cfgPath    string
        stateDir   string
        skipCargo  bool
    )
    cmd := &cobra.Command{
        Use:   "health",
        Short: "Validate config and surface recent errors. Exits non-zero on problems.",
        RunE: func(cmd *cobra.Command, args []string) error {
            if cfgPath == "" {
                cfgPath = config.DefaultPath()
            }
            cfg, err := config.Load(cfgPath)
            if err != nil {
                return fmt.Errorf("config load: %w", err)
            }
            if err := cfg.Validate(); err != nil {
                return fmt.Errorf("config invalid: %w", err)
            }
            for _, d := range cfg.ScanDirs {
                if _, err := os.Stat(d); err != nil {
                    return fmt.Errorf("scan_dir %s: %w", d, err)
                }
            }
            for _, d := range cfg.ProjectDirs {
                if _, err := os.Stat(filepath.Join(d, "Cargo.toml")); err != nil {
                    return fmt.Errorf("project_dir %s missing Cargo.toml: %w", d, err)
                }
            }
            if !skipCargo {
                if _, err := cleaner.ResolveCargoBin(cleaner.DefaultCargoCandidates()); err != nil {
                    return fmt.Errorf("cargo: %w", err)
                }
            }

            paths := config.Paths()
            if stateDir != "" {
                paths.DBPath = filepath.Join(stateDir, "state.db")
            }
            ctx := context.Background()
            s, err := store.Open(ctx, paths.DBPath)
            if err != nil {
                return fmt.Errorf("state db: %w", err)
            }
            defer s.Close()
            if err := s.Migrate(ctx); err != nil {
                return fmt.Errorf("state db migrate: %w", err)
            }

            since := time.Now().Add(-24 * time.Hour)
            errs, err := s.ErrorsSince(ctx, since)
            if err != nil {
                return err
            }
            fmt.Fprintln(cmd.OutOrStdout(), "OK")
            if len(errs) > 0 {
                fmt.Fprintf(cmd.OutOrStdout(), "WARN: %d errors in last 24h\n", len(errs))
            }
            return nil
        },
    }
    cmd.Flags().StringVar(&cfgPath, "config", "", "path to config file (default: $XDG_CONFIG_HOME/car-go-clean/config.toml)")
    cmd.Flags().StringVar(&stateDir, "state-dir", "", "override state dir (default from XDG_STATE_HOME)")
    cmd.Flags().BoolVar(&skipCargo, "skip-cargo", false, "skip the cargo-on-PATH check (useful in tests)")
    return cmd
}
```

- [ ] **Step 4: Register the command**

In `cmd/car-go-clean/root.go`, add inside `newRootCmd`:
```go
    cmd.AddCommand(newHealthCmd())
```

- [ ] **Step 5: Run — must pass**

```bash
go test ./cmd/car-go-clean/...
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add cmd/car-go-clean/
git commit -m "feat(cli): add health subcommand"
```

---

### Task 23: Add `config` and `status` subcommands

**Files:**
- Create: `cmd/car-go-clean/config_cmd.go`
- Create: `cmd/car-go-clean/status.go`
- Modify: `cmd/car-go-clean/root.go`

- [ ] **Step 1: Implement `config`**

`cmd/car-go-clean/config_cmd.go`:
```go
package main

import (
    "github.com/BurntSushi/toml"
    "github.com/spf13/cobra"

    "github.com/dcchuck/car-go-clean/internal/config"
)

func newConfigCmd() *cobra.Command {
    var path string
    cmd := &cobra.Command{
        Use:   "config",
        Short: "Print the effective configuration (defaults merged with file).",
        RunE: func(cmd *cobra.Command, args []string) error {
            if path == "" {
                path = config.DefaultPath()
            }
            cfg, err := config.Load(path)
            if err != nil {
                return err
            }
            return toml.NewEncoder(cmd.OutOrStdout()).Encode(cfg)
        },
    }
    cmd.Flags().StringVar(&path, "config", "", "path to config file")
    return cmd
}
```

- [ ] **Step 2: Implement `status`**

`cmd/car-go-clean/status.go`:
```go
package main

import (
    "context"
    "fmt"
    "path/filepath"
    "time"

    "github.com/spf13/cobra"

    "github.com/dcchuck/car-go-clean/internal/config"
    "github.com/dcchuck/car-go-clean/internal/store"
)

func newStatusCmd() *cobra.Command {
    var stateDir string
    cmd := &cobra.Command{
        Use:   "status",
        Short: "Show last run, cached project count, total bytes recovered.",
        RunE: func(cmd *cobra.Command, args []string) error {
            paths := config.Paths()
            if stateDir != "" {
                paths.DBPath = filepath.Join(stateDir, "state.db")
            }
            ctx := context.Background()
            s, err := store.Open(ctx, paths.DBPath)
            if err != nil {
                return err
            }
            defer s.Close()
            if err := s.Migrate(ctx); err != nil {
                return err
            }
            projects, _ := s.AllProjects(ctx)
            total, _ := s.TotalBytesRecovered(ctx, time.Time{})
            last, lastErr := s.LastRun(ctx)

            out := cmd.OutOrStdout()
            fmt.Fprintf(out, "Cached projects: %d\n", len(projects))
            fmt.Fprintf(out, "Total bytes recovered (all time): %d\n", total)
            if lastErr == nil {
                fmt.Fprintf(out, "Last run: started=%s finished=%v cleaned=%d recovered=%d errors=%d\n",
                    last.StartedAt.Format(time.RFC3339), last.FinishedAt, last.ProjectsCleaned, last.BytesRecovered, last.ErrorsCount)
            } else {
                fmt.Fprintln(out, "Last run: <none>")
            }
            return nil
        },
    }
    cmd.Flags().StringVar(&stateDir, "state-dir", "", "override state dir")
    return cmd
}
```

- [ ] **Step 3: Register**

In `root.go`, inside `newRootCmd`:
```go
    cmd.AddCommand(newConfigCmd())
    cmd.AddCommand(newStatusCmd())
```

- [ ] **Step 4: Smoke test**

```bash
make build && ./bin/car-go-clean config
./bin/car-go-clean status --state-dir "$(mktemp -d)"
```

Expected: `config` prints TOML defaults; `status` prints "Cached projects: 0", "Total bytes recovered (all time): 0", "Last run: <none>".

- [ ] **Step 5: Commit**

```bash
git add cmd/car-go-clean/
git commit -m "feat(cli): add config and status subcommands"
```

---

### Task 24: Add `scan` and `run` subcommands (acquire lock)

**Files:**
- Create: `cmd/car-go-clean/scan.go`
- Create: `cmd/car-go-clean/run.go`
- Modify: `cmd/car-go-clean/root.go`

- [ ] **Step 1: Implement a shared builder for daemon deps**

Append to `cmd/car-go-clean/root.go`:
```go
import (
    "context"
    "fmt"
    "log/slog"
    "path/filepath"

    "github.com/dcchuck/car-go-clean/internal/cache"
    "github.com/dcchuck/car-go-clean/internal/cleaner"
    "github.com/dcchuck/car-go-clean/internal/config"
    "github.com/dcchuck/car-go-clean/internal/daemon"
    "github.com/dcchuck/car-go-clean/internal/scanner"
    "github.com/dcchuck/car-go-clean/internal/store"
)

// buildDaemonDeps loads config from disk, validates it, and constructs a
// Daemon plus its Store. Used by scan/run; the daemon subcommand uses
// buildDaemonDepsWithConfig to avoid loading config twice.
func buildDaemonDeps(ctx context.Context, cfgPath, stateDir string, logger *slog.Logger) (*daemon.Daemon, *store.Store, config.Config, error) {
    if cfgPath == "" {
        cfgPath = config.DefaultPath()
    }
    cfg, err := config.Load(cfgPath)
    if err != nil {
        return nil, nil, cfg, fmt.Errorf("config: %w", err)
    }
    if err := cfg.Validate(); err != nil {
        return nil, nil, cfg, err
    }
    d, s, err := buildDaemonDepsWithConfig(ctx, cfg, stateDir, logger)
    return d, s, cfg, err
}

func buildDaemonDepsWithConfig(ctx context.Context, cfg config.Config, stateDir string, logger *slog.Logger) (*daemon.Daemon, *store.Store, error) {
    paths := config.Paths()
    if stateDir != "" {
        paths.DBPath = filepath.Join(stateDir, "state.db")
        paths.LockPath = filepath.Join(stateDir, "daemon.lock")
        paths.LogPath = filepath.Join(stateDir, "car-go-clean.log")
    }
    s, err := store.Open(ctx, paths.DBPath)
    if err != nil {
        return nil, nil, err
    }
    if err := s.Migrate(ctx); err != nil {
        s.Close()
        return nil, nil, err
    }
    cargoBin, err := cleaner.ResolveCargoBin(cleaner.DefaultCargoCandidates())
    if err != nil {
        s.Close()
        return nil, nil, err
    }
    d := daemon.New(daemon.Deps{
        Store:   s,
        Cache:   cache.New(s),
        Scanner: scanner.New(scanner.Options{
            Roots:       cfg.ScanDirs,
            ProjectDirs: cfg.ProjectDirs,
            Excludes:    cfg.Excludes,
        }),
        Cleaner: cleaner.New(cleaner.Options{Runner: cleaner.RealRunner(), CargoBin: cargoBin}),
        Clock:   daemon.RealClock(),
        Logger:  logger,
    })
    return d, s, nil
}
```

- [ ] **Step 2: Implement `scan`**

`cmd/car-go-clean/scan.go`:
```go
package main

import (
    "context"
    "fmt"
    "log/slog"
    "path/filepath"

    "github.com/spf13/cobra"

    "github.com/dcchuck/car-go-clean/internal/config"
    "github.com/dcchuck/car-go-clean/internal/lockfile"
)

func newScanCmd() *cobra.Command {
    var cfgPath, stateDir string
    cmd := &cobra.Command{
        Use:   "scan",
        Short: "Walk configured roots and refresh the project cache.",
        RunE: func(cmd *cobra.Command, args []string) error {
            paths := config.Paths()
            if stateDir != "" {
                paths.LockPath = filepath.Join(stateDir, "daemon.lock")
            }
            lk, err := lockfile.TryAcquire(paths.LockPath)
            if err != nil {
                return fmt.Errorf("another car-go-clean process is running: %w", err)
            }
            defer lk.Release()

            ctx := context.Background()
            d, s, _, err := buildDaemonDeps(ctx, cfgPath, stateDir, slog.Default())
            if err != nil {
                return err
            }
            defer s.Close()
            return d.ScanCycle(ctx)
        },
    }
    cmd.Flags().StringVar(&cfgPath, "config", "", "path to config file")
    cmd.Flags().StringVar(&stateDir, "state-dir", "", "override state dir")
    return cmd
}
```

- [ ] **Step 3: Implement `run`**

`cmd/car-go-clean/run.go`:
```go
package main

import (
    "context"
    "fmt"
    "log/slog"
    "path/filepath"

    "github.com/spf13/cobra"

    "github.com/dcchuck/car-go-clean/internal/config"
    "github.com/dcchuck/car-go-clean/internal/lockfile"
)

func newRunCmd() *cobra.Command {
    var cfgPath, stateDir string
    cmd := &cobra.Command{
        Use:   "run",
        Short: "Run one clean cycle synchronously.",
        RunE: func(cmd *cobra.Command, args []string) error {
            paths := config.Paths()
            if stateDir != "" {
                paths.LockPath = filepath.Join(stateDir, "daemon.lock")
            }
            lk, err := lockfile.TryAcquire(paths.LockPath)
            if err != nil {
                return fmt.Errorf("another car-go-clean process is running: %w", err)
            }
            defer lk.Release()

            ctx := context.Background()
            d, s, _, err := buildDaemonDeps(ctx, cfgPath, stateDir, slog.Default())
            if err != nil {
                return err
            }
            defer s.Close()
            return d.RunCycle(ctx)
        },
    }
    cmd.Flags().StringVar(&cfgPath, "config", "", "path to config file")
    cmd.Flags().StringVar(&stateDir, "state-dir", "", "override state dir")
    return cmd
}
```

- [ ] **Step 4: Register**

In `root.go`:
```go
    cmd.AddCommand(newScanCmd())
    cmd.AddCommand(newRunCmd())
```

- [ ] **Step 5: Build to confirm it compiles**

```bash
make build
```

Expected: clean build, no errors.

- [ ] **Step 6: Commit**

```bash
git add cmd/car-go-clean/
git commit -m "feat(cli): add scan and run subcommands with advisory lock"
```

---

### Task 25: Add `daemon` subcommand and signal handling

**Files:**
- Create: `cmd/car-go-clean/daemon.go`
- Modify: `cmd/car-go-clean/root.go`

- [ ] **Step 1: Implement `daemon`**

`cmd/car-go-clean/daemon.go`:
```go
package main

import (
    "context"
    "fmt"
    "os/signal"
    "path/filepath"
    "syscall"

    "github.com/spf13/cobra"

    "github.com/dcchuck/car-go-clean/internal/config"
    "github.com/dcchuck/car-go-clean/internal/daemon"
    "github.com/dcchuck/car-go-clean/internal/lockfile"
    "github.com/dcchuck/car-go-clean/internal/logging"
)

func newDaemonCmd() *cobra.Command {
    var cfgPath, stateDir string
    cmd := &cobra.Command{
        Use:   "daemon",
        Short: "Long-running scheduler. Typically invoked by launchd/systemd.",
        RunE: func(cmd *cobra.Command, args []string) error {
            paths := config.Paths()
            if stateDir != "" {
                paths.DBPath = filepath.Join(stateDir, "state.db")
                paths.LockPath = filepath.Join(stateDir, "daemon.lock")
                paths.LogPath = filepath.Join(stateDir, "car-go-clean.log")
            }

            lk, err := lockfile.TryAcquire(paths.LockPath)
            if err != nil {
                return fmt.Errorf("daemon already running: %w", err)
            }
            defer lk.Release()

            ctx, stop := signal.NotifyContext(cmd.Context(), syscall.SIGINT, syscall.SIGTERM)
            defer stop()

            if cfgPath == "" {
                cfgPath = config.DefaultPath()
            }
            cfg, err := config.Load(cfgPath)
            if err != nil {
                return err
            }
            if err := cfg.Validate(); err != nil {
                return err
            }
            logger, err := logging.New(logging.Options{LogPath: paths.LogPath, Level: cfg.LogLevel})
            if err != nil {
                return err
            }
            defer logger.Close()

            d, s, err := buildDaemonDepsWithConfig(ctx, cfg, stateDir, logger.Logger)
            if err != nil {
                return err
            }
            defer s.Close()

            logger.Info("daemon starting", "clean_interval", cfg.CleanInterval, "scan_interval", cfg.ScanInterval)
            err = d.Run(ctx, daemon.Config{CleanInterval: cfg.CleanInterval, ScanInterval: cfg.ScanInterval})
            if err != nil && err != context.Canceled {
                return err
            }
            logger.Info("daemon stopped")
            return nil
        },
    }
    cmd.Flags().StringVar(&cfgPath, "config", "", "path to config file")
    cmd.Flags().StringVar(&stateDir, "state-dir", "", "override state dir")
    return cmd
}
```

- [ ] **Step 2: Register**

In `root.go`, inside `newRootCmd`:
```go
    cmd.AddCommand(newDaemonCmd())
```

- [ ] **Step 3: Confirm build**

```bash
make build && ./bin/car-go-clean --help
```

Expected: help text lists `daemon`, `health`, `run`, `scan`, `status`, `config`, `version`.

- [ ] **Step 4: Commit**

```bash
git add cmd/car-go-clean/
git commit -m "feat(cli): add daemon subcommand with signal handling"
```

---

### Task 26: Add `stats` subcommand

**Files:**
- Create: `cmd/car-go-clean/stats.go`
- Modify: `cmd/car-go-clean/root.go`

- [ ] **Step 1: Implement**

`cmd/car-go-clean/stats.go`:
```go
package main

import (
    "context"
    "encoding/json"
    "fmt"
    "path/filepath"
    "strconv"
    "time"

    "github.com/spf13/cobra"

    "github.com/dcchuck/car-go-clean/internal/config"
    "github.com/dcchuck/car-go-clean/internal/store"
)

// parseSince accepts standard Go durations ("24h", "30m") and also the
// extended suffixes "d" (days) and "w" (weeks) that time.ParseDuration
// rejects but that users naturally type ("--since 7d").
func parseSince(s string) (time.Duration, error) {
    if s == "" {
        return 0, nil
    }
    if len(s) > 1 {
        last := s[len(s)-1]
        if last == 'd' || last == 'w' {
            n, err := strconv.Atoi(s[:len(s)-1])
            if err != nil {
                return 0, fmt.Errorf("--since %q: %w", s, err)
            }
            unit := 24 * time.Hour
            if last == 'w' {
                unit = 7 * 24 * time.Hour
            }
            return time.Duration(n) * unit, nil
        }
    }
    return time.ParseDuration(s)
}

func newStatsCmd() *cobra.Command {
    var (
        sinceFlag string
        topN      int
        asJSON    bool
        stateDir  string
    )
    cmd := &cobra.Command{
        Use:   "stats",
        Short: "Show disk space recovered over time.",
        RunE: func(cmd *cobra.Command, args []string) error {
            since := time.Time{}
            d, err := parseSince(sinceFlag)
            if err != nil {
                return err
            }
            if d > 0 {
                since = time.Now().Add(-d)
            }
            paths := config.Paths()
            if stateDir != "" {
                paths.DBPath = filepath.Join(stateDir, "state.db")
            }
            ctx := context.Background()
            s, err := store.Open(ctx, paths.DBPath)
            if err != nil {
                return err
            }
            defer s.Close()
            if err := s.Migrate(ctx); err != nil {
                return err
            }
            total, err := s.TotalBytesRecovered(ctx, since)
            if err != nil {
                return err
            }
            top, err := s.TopProjectsByBytes(ctx, since, topN)
            if err != nil {
                return err
            }
            if asJSON {
                return json.NewEncoder(cmd.OutOrStdout()).Encode(map[string]any{
                    "since":            since.Format(time.RFC3339),
                    "total_bytes":      total,
                    "top_projects":     top,
                })
            }
            fmt.Fprintf(cmd.OutOrStdout(), "Bytes recovered since %s: %d\n",
                since.Format(time.RFC3339), total)
            for i, p := range top {
                fmt.Fprintf(cmd.OutOrStdout(), "  %d. %s — %d bytes\n", i+1, p.Path, p.Bytes)
            }
            return nil
        },
    }
    cmd.Flags().StringVar(&sinceFlag, "since", "", "duration to look back (e.g. 7d, 24h, 4w). empty = all time")
    cmd.Flags().IntVar(&topN, "top", 10, "show top-N projects by bytes recovered")
    cmd.Flags().BoolVar(&asJSON, "json", false, "emit JSON instead of human text")
    cmd.Flags().StringVar(&stateDir, "state-dir", "", "override state dir")
    return cmd
}
```

- [ ] **Step 2: Register**

In `root.go`:
```go
    cmd.AddCommand(newStatsCmd())
```

- [ ] **Step 3: Smoke test**

```bash
make build && ./bin/car-go-clean stats --state-dir "$(mktemp -d)"
```

Expected: prints `Bytes recovered since ...: 0` (no top entries since empty DB).

- [ ] **Step 4: Commit**

```bash
git add cmd/car-go-clean/
git commit -m "feat(cli): add stats subcommand"
```

---

### Task 27: Add `logs` subcommand

**Files:**
- Create: `cmd/car-go-clean/logs.go`
- Modify: `cmd/car-go-clean/root.go`

- [ ] **Step 1: Implement**

`cmd/car-go-clean/logs.go`:
```go
package main

import (
    "bufio"
    "context"
    "fmt"
    "io"
    "os"
    "path/filepath"
    "time"

    "github.com/spf13/cobra"

    "github.com/dcchuck/car-go-clean/internal/config"
    "github.com/dcchuck/car-go-clean/internal/store"
)

func newLogsCmd() *cobra.Command {
    var (
        errorsOnly bool
        tailN      int
        stateDir   string
    )
    cmd := &cobra.Command{
        Use:   "logs",
        Short: "Tail the daemon log file or show recent errors.",
        RunE: func(cmd *cobra.Command, args []string) error {
            paths := config.Paths()
            if stateDir != "" {
                paths.LogPath = filepath.Join(stateDir, "car-go-clean.log")
                paths.DBPath = filepath.Join(stateDir, "state.db")
            }
            if errorsOnly {
                ctx := context.Background()
                s, err := store.Open(ctx, paths.DBPath)
                if err != nil {
                    return err
                }
                defer s.Close()
                if err := s.Migrate(ctx); err != nil {
                    return err
                }
                errs, err := s.ErrorsSince(ctx, time.Now().Add(-7*24*time.Hour))
                if err != nil {
                    return err
                }
                for _, e := range errs {
                    fmt.Fprintf(cmd.OutOrStdout(), "%s [%s] %s — %s\n",
                        e.TS.Format(time.RFC3339), e.Category, e.Path, e.Message)
                }
                return nil
            }
            return tailFile(cmd.OutOrStdout(), paths.LogPath, tailN)
        },
    }
    cmd.Flags().BoolVar(&errorsOnly, "errors-only", false, "show recent errors from state DB instead of log file")
    cmd.Flags().IntVar(&tailN, "tail", 100, "number of trailing lines to show")
    cmd.Flags().StringVar(&stateDir, "state-dir", "", "override state dir")
    return cmd
}

// tailFile prints the last n lines of path to w.
func tailFile(w io.Writer, path string, n int) error {
    f, err := os.Open(path)
    if err != nil {
        return err
    }
    defer f.Close()
    sc := bufio.NewScanner(f)
    sc.Buffer(make([]byte, 0, 1024*1024), 1024*1024)
    var lines []string
    for sc.Scan() {
        lines = append(lines, sc.Text())
        if len(lines) > n {
            lines = lines[1:]
        }
    }
    if err := sc.Err(); err != nil {
        return err
    }
    for _, l := range lines {
        fmt.Fprintln(w, l)
    }
    return nil
}
```

- [ ] **Step 2: Register**

In `root.go`:
```go
    cmd.AddCommand(newLogsCmd())
```

- [ ] **Step 3: Build**

```bash
make build && ./bin/car-go-clean logs --help
```

Expected: help text printed.

- [ ] **Step 4: Commit**

```bash
git add cmd/car-go-clean/
git commit -m "feat(cli): add logs subcommand"
```

---

---

## Chunk 6: End-to-End Test and Distribution

This chunk lands the end-to-end smoke test, the launchd/systemd service files, the Goreleaser config that emits the Homebrew formula, and finishes with a README and license.

### Task 28: End-to-end smoke test

**Files:**
- Create: `cmd/car-go-clean/e2e_test.go`

- [ ] **Step 1: Write the e2e test**

`cmd/car-go-clean/e2e_test.go`:
```go
package main

import (
    "bytes"
    "os"
    "os/exec"
    "path/filepath"
    "strings"
    "testing"
)

// Build the binary and exercise scan + run + stats end-to-end against a
// fake cargo on PATH and a synthetic project tree.
func TestEndToEnd_ScanRunStats(t *testing.T) {
    if testing.Short() {
        t.Skip("skipping e2e in short mode")
    }
    repoRoot, err := exec.Command("git", "rev-parse", "--show-toplevel").Output()
    if err != nil {
        t.Fatalf("git rev-parse: %v", err)
    }
    repo := strings.TrimSpace(string(repoRoot))

    work := t.TempDir()
    binDir := filepath.Join(work, "bin")
    if err := os.MkdirAll(binDir, 0o755); err != nil {
        t.Fatal(err)
    }
    binPath := filepath.Join(binDir, "car-go-clean")
    build := exec.Command("go", "build", "-o", binPath, "./cmd/car-go-clean")
    build.Dir = repo
    if out, err := build.CombinedOutput(); err != nil {
        t.Fatalf("build: %v\n%s", err, out)
    }

    // Synthetic project tree.
    proj := filepath.Join(work, "tree", "proj")
    if err := os.MkdirAll(filepath.Join(proj, "target", "debug"), 0o755); err != nil {
        t.Fatal(err)
    }
    if err := os.WriteFile(filepath.Join(proj, "Cargo.toml"), []byte("[package]\nname=\"x\"\nversion=\"0.1.0\"\n"), 0o644); err != nil {
        t.Fatal(err)
    }
    if err := os.WriteFile(filepath.Join(proj, "target", "debug", "blob.bin"), make([]byte, 16*1024), 0o644); err != nil {
        t.Fatal(err)
    }

    // Fake cargo: shell script that wipes target/ when invoked with "clean".
    fakeCargo := filepath.Join(binDir, "cargo")
    script := "#!/bin/sh\nif [ \"$1\" = clean ]; then rm -rf target; fi\n"
    if err := os.WriteFile(fakeCargo, []byte(script), 0o755); err != nil {
        t.Fatal(err)
    }

    stateDir := filepath.Join(work, "state")
    cfgPath := filepath.Join(work, "config.toml")
    if err := os.WriteFile(cfgPath, []byte("scan_dirs = [\""+filepath.Join(work, "tree")+"\"]\n"), 0o644); err != nil {
        t.Fatal(err)
    }

    runBin := func(args ...string) (string, error) {
        cmd := exec.Command(binPath, args...)
        cmd.Env = append(os.Environ(), "PATH="+binDir+":"+os.Getenv("PATH"))
        var buf bytes.Buffer
        cmd.Stdout = &buf
        cmd.Stderr = &buf
        err := cmd.Run()
        return buf.String(), err
    }

    if out, err := runBin("scan", "--config", cfgPath, "--state-dir", stateDir); err != nil {
        t.Fatalf("scan: %v\n%s", err, out)
    }
    if out, err := runBin("run", "--config", cfgPath, "--state-dir", stateDir); err != nil {
        t.Fatalf("run: %v\n%s", err, out)
    }
    out, err := runBin("stats", "--state-dir", stateDir)
    if err != nil {
        t.Fatalf("stats: %v\n%s", err, out)
    }
    if !strings.Contains(out, "Bytes recovered") {
        t.Fatalf("unexpected stats output: %s", out)
    }
    if !strings.Contains(out, proj) {
        t.Fatalf("project missing from stats: %s", out)
    }
}
```

- [ ] **Step 2: Run**

```bash
go test ./cmd/car-go-clean/... -timeout 60s
```

Expected: PASS (including e2e).

- [ ] **Step 3: Commit**

```bash
git add cmd/car-go-clean/
git commit -m "test(cli): add scan→run→stats e2e smoke test"
```

---

### Task 29: Service unit files and Goreleaser

**Files:**
- Create: `contrib/launchd/com.dcchuck.car-go-clean.plist`
- Create: `contrib/systemd/car-go-clean.service`
- Create: `.goreleaser.yaml`

- [ ] **Step 1: launchd plist**

`contrib/launchd/com.dcchuck.car-go-clean.plist`:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.dcchuck.car-go-clean</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/homebrew/bin/car-go-clean</string>
    <string>daemon</string>
  </array>
  <key>KeepAlive</key><true/>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>/tmp/car-go-clean.out.log</string>
  <key>StandardErrorPath</key><string>/tmp/car-go-clean.err.log</string>
</dict>
</plist>
```

- [ ] **Step 2: systemd unit**

`contrib/systemd/car-go-clean.service`:
```ini
[Unit]
Description=car-go-clean: periodically cargo clean Rust projects
After=network.target

[Service]
ExecStart=%h/.local/bin/car-go-clean daemon
Restart=on-failure
RestartSec=10s

[Install]
WantedBy=default.target
```

(This is a user-unit; installed via `systemctl --user`.)

- [ ] **Step 3: Goreleaser config**

`.goreleaser.yaml`:
```yaml
version: 2
project_name: car-go-clean

before:
  hooks:
    - go mod tidy

builds:
  - id: car-go-clean
    main: ./cmd/car-go-clean
    binary: car-go-clean
    env: [CGO_ENABLED=0]
    goos: [darwin, linux]
    goarch: [amd64, arm64]
    ldflags:
      - -s -w -X main.version={{.Version}}

archives:
  - id: default
    formats: [tar.gz]
    files:
      - LICENSE*
      - README.md
      - contrib/**/*

brews:
  - name: car-go-clean
    repository:
      owner: dcchuck
      name: homebrew-tap
    homepage: https://github.com/dcchuck/car-go-clean
    description: Periodically run cargo clean on all Rust projects on your machine.
    service: |
      run [opt_bin/"car-go-clean", "daemon"]
      keep_alive true
      log_path var/"log/car-go-clean.log"
      error_log_path var/"log/car-go-clean.err.log"

checksum:
  name_template: "checksums.txt"

snapshot:
  version_template: "{{ incpatch .Version }}-next"
```

- [ ] **Step 4: Smoke check goreleaser config locally (optional)**

If goreleaser is installed locally:
```bash
goreleaser check
```

Expected: `valid configuration`.

If not installed, skip.

- [ ] **Step 5: Commit**

```bash
git add contrib/ .goreleaser.yaml
git commit -m "build: add launchd plist, systemd unit, and goreleaser config"
```

---

### Task 30: README, license, and final pass

**Files:**
- Modify: `README.md`
- Create: `LICENSE`

- [ ] **Step 1: Expand the README**

Overwrite `README.md` with the following content (the outer fence below uses **four** backticks so that the nested triple-backtick code blocks inside the README do not terminate it prematurely; copy the bytes *between* the four-backtick markers):

````markdown
# car-go-clean

A small daemon that periodically runs `cargo clean` on every Rust project
on your machine, tracking how much disk space it has reclaimed over time.

## Install

```bash
brew install dcchuck/tap/car-go-clean
brew services start car-go-clean
```

Or from source:

```bash
go install github.com/dcchuck/car-go-clean/cmd/car-go-clean@latest
car-go-clean daemon &  # or use the systemd user-unit in contrib/
```

## Configure (optional)

`~/.config/car-go-clean/config.toml`:

```toml
scan_dirs      = ["~/code", "~/work"]
project_dirs   = ["~/play/one-off-project"]
clean_interval = "24h"
scan_interval  = "168h"
log_level      = "info"
```

## Commands

| Command                | What it does                              |
|------------------------|-------------------------------------------|
| `car-go-clean daemon`  | Long-running scheduler (used by services). |
| `car-go-clean scan`    | Re-walk roots; refresh the project cache. |
| `car-go-clean run`     | Run a single clean cycle now.            |
| `car-go-clean health`  | Validate config and surface recent errors. |
| `car-go-clean status`  | Show last run, cached project count.     |
| `car-go-clean stats`   | Disk recovered over time / top projects. |
| `car-go-clean logs`    | Tail logs or show recent errors.         |
| `car-go-clean config`  | Print effective configuration.           |
| `car-go-clean version` | Print version.                            |

See `docs/superpowers/specs/` for the full design.
````

- [ ] **Step 2: Add MIT LICENSE**

`LICENSE`:
```
MIT License

Copyright (c) 2026 Chuck Danielsson

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

- [ ] **Step 3: Run the full test suite**

```bash
go test -race ./...
```

Expected: all packages pass with race detector on.

- [ ] **Step 4: Run `go vet`**

```bash
go vet ./...
```

Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add README.md LICENSE
git commit -m "docs: expand README and add MIT license"
```

---

## Done Criteria

- [ ] `go test -race ./...` passes.
- [ ] `go vet ./...` is clean.
- [ ] `make build` produces a working `bin/car-go-clean` binary.
- [ ] `./bin/car-go-clean health --state-dir <empty> --skip-cargo` exits 0 with output `OK`.
- [ ] e2e test in `cmd/car-go-clean/e2e_test.go` passes against a fake `cargo` on PATH.
- [ ] All checkboxes in this plan are checked.
