use serde::Serialize;
use std::collections::BTreeSet;
use std::path::PathBuf;

pub const FORMAT_VERSION: u32 = 1;

pub mod reason {
    pub const CARGO_FAILED: &str = "cargo_failed";
    pub const CLEANUP_FAILED: &str = "cleanup_failed";
    pub const COMMAND_FAILED: &str = "command_failed";
    pub const GENERATION_INVALID: &str = "generation_invalid";
    pub const GENERATION_MISSING: &str = "generation_missing";
    pub const LOCK_UNAVAILABLE: &str = "lock_unavailable";
    pub const MEASUREMENT_FAILED: &str = "measurement_failed";
    pub const ORIGIN_INCOMPLETE: &str = "origin_incomplete";
    pub const REVIEW_GENERATION_MISMATCH: &str = "review_generation_mismatch";
    pub const REVIEW_PLAN_EXPIRED: &str = "review_plan_expired";
    pub const REVIEW_PLAN_MISSING: &str = "review_plan_missing";
    pub const REVIEW_POLICY_MISMATCH: &str = "review_policy_mismatch";
    pub const SCAN_INCOMPLETE: &str = "scan_incomplete";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    Complete,
    Failed,
    Incomplete,
}

impl CommandOutcome {
    pub fn merge(self, other: Self) -> Self {
        use CommandOutcome::{Complete, Failed, Incomplete};
        match (self, other) {
            (Failed, _) | (_, Failed) => Failed,
            (Incomplete, _) | (_, Incomplete) => Incomplete,
            (Complete, Complete) => Complete,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Complete => 0,
            Self::Failed => 1,
            Self::Incomplete => 2,
        }
    }

    pub fn kind(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandStatus {
    outcome: CommandOutcome,
    reasons: BTreeSet<String>,
}

impl CommandStatus {
    pub fn complete() -> Self {
        Self {
            outcome: CommandOutcome::Complete,
            reasons: BTreeSet::new(),
        }
    }

    pub fn incomplete(reason: impl Into<String>) -> Self {
        Self::complete().merge_reason(CommandOutcome::Incomplete, reason)
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        Self::complete().merge_reason(CommandOutcome::Failed, reason)
    }

    pub fn merge(mut self, other: Self) -> Self {
        self.outcome = self.outcome.merge(other.outcome);
        self.reasons.extend(other.reasons);
        self
    }

    pub fn merge_reason(mut self, outcome: CommandOutcome, reason: impl Into<String>) -> Self {
        self.outcome = self.outcome.merge(outcome);
        self.reasons.insert(reason.into());
        self
    }

    pub fn outcome(&self) -> CommandOutcome {
        self.outcome
    }

    pub fn report(&self) -> OutcomeReport {
        OutcomeReport {
            code: self.outcome.code(),
            kind: self.outcome.kind(),
            reasons: self.reasons.iter().cloned().collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OutcomeReport {
    pub code: u8,
    pub kind: &'static str,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScanErrorReport {
    pub kind: String,
    pub path: Option<PathBuf>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandReport<T> {
    pub format_version: u32,
    pub command: &'static str,
    pub outcome: OutcomeReport,
    pub policy_hash: Option<String>,
    pub generation: Option<i64>,
    pub review_id: Option<i64>,
    pub scan_errors: Vec<ScanErrorReport>,
    pub data: T,
}

impl<T> CommandReport<T> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        command: &'static str,
        status: &CommandStatus,
        policy_hash: Option<String>,
        generation: Option<i64>,
        review_id: Option<i64>,
        scan_errors: Vec<ScanErrorReport>,
        data: T,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            command,
            outcome: status.report(),
            policy_hash,
            generation,
            review_id,
            scan_errors,
            data,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamEvent<T> {
    pub format_version: u32,
    pub event: &'static str,
    pub data: T,
}

impl<T> StreamEvent<T> {
    pub fn new(event: &'static str, data: T) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            event,
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_outranks_incomplete_and_incomplete_outranks_complete() {
        assert_eq!(
            CommandOutcome::Complete.merge(CommandOutcome::Incomplete),
            CommandOutcome::Incomplete
        );
        assert_eq!(
            CommandOutcome::Incomplete.merge(CommandOutcome::Failed),
            CommandOutcome::Failed
        );
        assert_eq!(
            CommandOutcome::Failed.merge(CommandOutcome::Complete),
            CommandOutcome::Failed
        );
    }

    #[test]
    fn public_codes_are_zero_one_two() {
        assert_eq!(CommandOutcome::Complete.code(), 0);
        assert_eq!(CommandOutcome::Failed.code(), 1);
        assert_eq!(CommandOutcome::Incomplete.code(), 2);
    }

    #[test]
    fn status_deduplicates_and_sorts_reasons_while_preserving_severity() {
        let status = CommandStatus::incomplete(reason::SCAN_INCOMPLETE)
            .merge(CommandStatus::failed(reason::CARGO_FAILED))
            .merge(CommandStatus::incomplete(reason::SCAN_INCOMPLETE));

        assert_eq!(status.outcome(), CommandOutcome::Failed);
        assert_eq!(
            status.report().reasons,
            vec![
                reason::CARGO_FAILED.to_string(),
                reason::SCAN_INCOMPLETE.to_string()
            ]
        );
    }
}
