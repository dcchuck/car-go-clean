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
}
