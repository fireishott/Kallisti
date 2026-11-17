//! Turn machine. Skeleton only in Wave 0; scheduling, the worker lease, and
//! cancellation land in Wave 8 (P8).
//!
//! The states are exported now because the device surface and the event log both
//! have to name them, and a second copy of this vocabulary would drift.

/// Where a turn is in its lifecycle.
///
/// `Queued` and `Running` are the states that can still be cancelled. `Done`,
/// `Failed`, and `Cancelled` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnState {
    /// Accepted and waiting for a worker.
    Queued,
    /// A worker holds the lease and the provider stream is open.
    Running,
    /// The final text is assembled and stored.
    Done,
    /// The turn ended with an error code.
    Failed,
    /// The customer cancelled before completion.
    Cancelled,
}

impl TurnState {
    /// Whether the turn has finished, whatever the outcome.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// Whether a cancel request can still take effect.
    #[must_use]
    pub const fn is_cancellable(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_in_flight_turns_are_cancellable() {
        assert!(TurnState::Queued.is_cancellable());
        assert!(TurnState::Running.is_cancellable());
        for state in [TurnState::Done, TurnState::Failed, TurnState::Cancelled] {
            assert!(state.is_terminal());
            assert!(!state.is_cancellable());
        }
    }
}
