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
