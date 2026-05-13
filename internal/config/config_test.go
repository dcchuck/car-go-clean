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
