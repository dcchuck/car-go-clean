use anyhow::Result;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use sysinfo::System;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySignal {
    pub pid: u32,
    pub project_path: PathBuf,
    pub reason: String,
}

pub trait ProcessInspector {
    fn active_projects(&self, projects: &[PathBuf]) -> Result<Vec<ActivitySignal>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopProcessInspector;

impl ProcessInspector for NoopProcessInspector {
    fn active_projects(&self, _projects: &[PathBuf]) -> Result<Vec<ActivitySignal>> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SysinfoProcessInspector;

impl ProcessInspector for SysinfoProcessInspector {
    fn active_projects(&self, projects: &[PathBuf]) -> Result<Vec<ActivitySignal>> {
        let system = System::new_all();
        let mut signals = Vec::new();

        for (pid, process) in system.processes() {
            let cwd = process.cwd();
            let args: Vec<PathBuf> = process
                .cmd()
                .iter()
                .map(|arg| PathBuf::from(arg.as_os_str()))
                .collect();

            signals.extend(activity_signals_for_process(
                pid.as_u32(),
                cwd,
                &args,
                projects,
            ));
        }

        Ok(signals)
    }
}

pub fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

pub fn process_matches_project(cwd: Option<&Path>, args: &[PathBuf], project: &Path) -> bool {
    if cwd.is_some_and(|cwd| path_is_within(cwd, project)) {
        return true;
    }

    let target = project.join("target");
    if args
        .iter()
        .any(|arg| argument_references_path(arg, project) || argument_references_path(arg, &target))
    {
        return true;
    }

    let Ok(canonical_project) = fs::canonicalize(project) else {
        return false;
    };
    if cwd
        .and_then(|cwd| fs::canonicalize(cwd).ok())
        .is_some_and(|cwd| path_is_within(&cwd, &canonical_project))
    {
        return true;
    }

    canonical_arguments_match_project(args, cwd, &canonical_project)
}

fn canonical_arguments_match_project(
    args: &[PathBuf],
    cwd: Option<&Path>,
    canonical_project: &Path,
) -> bool {
    args.iter().enumerate().any(|(index, arg)| {
        if canonical_argument_path(arg, cwd)
            .is_some_and(|arg| path_is_within(&arg, canonical_project))
        {
            return true;
        }

        let Some(option) = arg.to_str() else {
            return false;
        };
        match option {
            "--manifest-path" | "--target-dir" | "--out-dir" => args
                .get(index + 1)
                .and_then(|value| split_path_option_value(value))
                .and_then(|value| canonicalize_argument_path(value, cwd))
                .is_some_and(|value| path_is_within(&value, canonical_project)),
            "-L" | "--library-path" => args
                .get(index + 1)
                .and_then(|value| split_path_option_value(value))
                .and_then(rust_library_search_path)
                .and_then(|value| canonicalize_argument_path(value, cwd))
                .is_some_and(|value| path_is_within(&value, canonical_project)),
            "--extern" => args.get(index + 1).is_some_and(|value| {
                nested_rust_paths_match(value, RustPathSyntax::Extern, cwd, canonical_project)
            }),
            "--emit" => args.get(index + 1).is_some_and(|value| {
                nested_rust_paths_match(value, RustPathSyntax::Emit, cwd, canonical_project)
            }),
            _ => {
                combined_rust_library_search_path(option)
                    .and_then(|value| canonicalize_argument_path(value, cwd))
                    .is_some_and(|value| path_is_within(&value, canonical_project))
                    || option.strip_prefix("--extern=").is_some_and(|value| {
                        nested_rust_value_matches(
                            value,
                            RustPathSyntax::Extern,
                            cwd,
                            canonical_project,
                        )
                    })
                    || option.strip_prefix("--emit=").is_some_and(|value| {
                        nested_rust_value_matches(
                            value,
                            RustPathSyntax::Emit,
                            cwd,
                            canonical_project,
                        )
                    })
            }
        }
    })
}

fn canonical_argument_path(arg: &Path, cwd: Option<&Path>) -> Option<PathBuf> {
    let path = explicit_argument_path(arg).or_else(|| raw_argument_path(arg, cwd))?;
    canonicalize_argument_path(path, cwd)
}

fn canonicalize_argument_path(path: &Path, cwd: Option<&Path>) -> Option<PathBuf> {
    let mut path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd?.join(path)
    };
    let mut unresolved_suffix = Vec::<OsString>::new();
    loop {
        if let Ok(mut canonical) = fs::canonicalize(&path) {
            for component in unresolved_suffix.iter().rev() {
                canonical.push(component);
            }
            return Some(canonical);
        }
        unresolved_suffix.push(path.file_name()?.to_os_string());
        path = path.parent()?.to_path_buf();
    }
}

fn split_path_option_value(value: &Path) -> Option<&Path> {
    if value.as_os_str().is_empty() || value.to_str().is_some_and(|value| value.starts_with('-')) {
        return None;
    }
    Some(value)
}

fn rust_library_search_path(value: &Path) -> Option<&Path> {
    let Some(value_str) = value.to_str() else {
        return Some(value);
    };
    let Some((kind, path)) = value_str.split_once('=') else {
        return Some(value);
    };
    if !matches!(
        kind,
        "dependency" | "crate" | "native" | "framework" | "all"
    ) || path.is_empty()
    {
        return None;
    }
    Some(Path::new(path))
}

fn combined_rust_library_search_path(argument: &str) -> Option<&Path> {
    let value = argument
        .strip_prefix("--library-path=")
        .or_else(|| argument.strip_prefix("-L"))?;
    let value = split_path_option_value(Path::new(value))?;
    rust_library_search_path(value)
}

#[derive(Clone, Copy)]
enum RustPathSyntax {
    Extern,
    Emit,
}

fn nested_rust_paths_match(
    value: &Path,
    syntax: RustPathSyntax,
    cwd: Option<&Path>,
    canonical_project: &Path,
) -> bool {
    value
        .to_str()
        .is_some_and(|value| nested_rust_value_matches(value, syntax, cwd, canonical_project))
}

fn nested_rust_value_matches(
    value: &str,
    syntax: RustPathSyntax,
    cwd: Option<&Path>,
    canonical_project: &Path,
) -> bool {
    let path_matches = |path: &Path| {
        canonicalize_argument_path(path, cwd)
            .is_some_and(|path| path_is_within(&path, canonical_project))
    };
    match syntax {
        RustPathSyntax::Extern => nested_rust_path(value, syntax).is_some_and(path_matches),
        RustPathSyntax::Emit => value
            .split(',')
            .filter_map(|value| nested_rust_path(value, syntax))
            .any(path_matches),
    }
}

fn nested_rust_path(value: &str, syntax: RustPathSyntax) -> Option<&Path> {
    let (kind, path) = value.split_once('=')?;
    let valid_kind = match syntax {
        RustPathSyntax::Extern => !kind.is_empty(),
        RustPathSyntax::Emit => matches!(
            kind,
            "asm" | "llvm-bc" | "llvm-ir" | "obj" | "metadata" | "link" | "dep-info" | "mir"
        ),
    };
    if !valid_kind || path.is_empty() {
        return None;
    }
    Some(Path::new(path))
}

fn explicit_argument_path(arg: &Path) -> Option<&Path> {
    let arg = arg.to_str()?;
    if arg.starts_with("-L") || arg.starts_with("--library-path=") {
        return None;
    }
    let (option, value) = arg.split_once('=')?;
    if value.is_empty() {
        return None;
    }

    let path = Path::new(value);
    let known_path_option = matches!(option, "--manifest-path" | "--target-dir" | "--out-dir");
    let path_like_option_value = option.starts_with('-')
        && option.len() > 1
        && (path.is_absolute() || path.components().count() > 1);

    (known_path_option || path_like_option_value).then_some(path)
}

fn raw_argument_path<'a>(arg: &'a Path, cwd: Option<&Path>) -> Option<&'a Path> {
    if arg.is_absolute() || (cwd.is_some() && arg.components().count() > 1) {
        Some(arg)
    } else {
        None
    }
}

fn argument_references_path(arg: &Path, root: &Path) -> bool {
    if path_is_within(arg, root) {
        return true;
    }

    let arg = arg.to_string_lossy();
    let root = root.to_string_lossy();
    contains_path_prefix(&arg, &root)
}

fn contains_path_prefix(value: &str, root: &str) -> bool {
    if root.is_empty() {
        return false;
    }
    let mut rest = value;
    while let Some(offset) = rest.find(root) {
        let after = &rest[offset + root.len()..];
        if after
            .chars()
            .next()
            .is_none_or(|ch| ch == '/' || ch == '\\')
        {
            return true;
        }
        let advance = after.chars().next().map(char::len_utf8).unwrap_or(0);
        rest = &after[advance..];
    }
    false
}

pub fn activity_signals_for_process(
    pid: u32,
    cwd: Option<&Path>,
    args: &[PathBuf],
    projects: &[PathBuf],
) -> Vec<ActivitySignal> {
    projects
        .iter()
        .filter(|project| process_matches_project(cwd, args, project))
        .map(|project| ActivitySignal {
            pid,
            project_path: project.clone(),
            reason: "cwd or command references project".to_string(),
        })
        .collect()
}
