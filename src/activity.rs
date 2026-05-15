use anyhow::Result;
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
    args.iter()
        .any(|arg| argument_references_path(arg, project) || argument_references_path(arg, &target))
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
