//! The claim rate limiter: five attempts per ten minutes, then 429.
//!
//! It guards one form against a patient guesser, so the whole state is a map of
//! deques of instants in this process. That is deliberate: a restart clears it,
//! a restart also rebinds the port, and persisting a counter would mean writing
//! on every failed claim into the same database the claim is trying to unlock.
//!
//! The map is keyed by the effective client, which is the peer for a direct
//! request and the forwarded address for a request through a trusted proxy. One
//! proxy in front of the daemon therefore cannot pool every client into a single
//! budget, and a client cannot escape its own budget by arriving through a proxy.
//!
//! A refused attempt is not recorded, so the window cannot be extended forever by
//! knocking: five tries, then a ten-minute wait from the fifth.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Attempts allowed per window.
pub const CLAIM_LIMIT: usize = 5;

/// The window those attempts are counted in.
pub const CLAIM_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A sliding-window limiter for the one claim form, one budget per client.
#[derive(Debug)]
pub struct ClaimLimiter {
    inner: Mutex<HashMap<Option<IpAddr>, VecDeque<Instant>>>,
    limit: usize,
    window: Duration,
}

impl Default for ClaimLimiter {
    fn default() -> Self {
        Self::new(CLAIM_LIMIT, CLAIM_WINDOW)
    }
}

impl ClaimLimiter {
    /// A limiter with an explicit budget. Tests use a zero window.
    #[must_use]
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            limit,
            window,
        }
    }

    /// Record one attempt for the anonymous bucket. `false` means the caller must
    /// answer 429.
    ///
    /// Kept for the engine-level tests and for any caller that has no client
    /// context; the handlers pass the effective client to [`Self::allow_for`].
    pub fn allow(&self) -> bool {
        self.allow_for(None)
    }

    /// Record one attempt for one client. `false` means 429 for that client only.
    pub fn allow_for(&self, client: Option<IpAddr>) -> bool {
        let now = Instant::now();
        let mut budgets = match self.inner.lock() {
            Ok(guard) => guard,
            // A poisoned limiter is a bug, not a security hole: refuse rather
            // than silently allowing unlimited attempts.
            Err(_) => return false,
        };
        let attempts = budgets.entry(client).or_default();
        while let Some(oldest) = attempts.front() {
            if now.duration_since(*oldest) >= self.window {
                attempts.pop_front();
            } else {
                break;
            }
        }
        if attempts.len() >= self.limit {
            return false;
        }
        attempts.push_back(now);
        true
    }

    /// How many attempts are inside the window right now, across every client.
    #[must_use]
    pub fn attempts(&self) -> usize {
        match self.inner.lock() {
            Ok(guard) => guard.values().map(VecDeque::len).sum(),
            Err(_) => self.limit,
        }
    }

    /// The configured budget.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_budget_is_five_per_ten_minutes() {
        let limiter = ClaimLimiter::default();
        assert_eq!(limiter.limit(), 5);
        assert_eq!(CLAIM_WINDOW, Duration::from_secs(600));
    }

    #[test]
    fn the_sixth_attempt_inside_the_window_is_refused() {
        let limiter = ClaimLimiter::new(5, Duration::from_secs(600));
        for attempt in 1..=5 {
            assert!(limiter.allow(), "attempt {attempt} must be allowed");
        }
        assert!(!limiter.allow(), "the sixth attempt must be refused");
        assert_eq!(limiter.attempts(), 5, "a refusal is not recorded");
    }

    #[test]
    fn the_window_slides() {
        // A zero window means every attempt has already expired, so the limiter
        // never refuses. That is the reset behaviour without sleeping.
        let limiter = ClaimLimiter::new(2, Duration::ZERO);
        for _ in 0..50 {
            assert!(limiter.allow());
        }
        assert_eq!(
            limiter.attempts(),
            1,
            "the deque is pruned to the current window"
        );
    }
}
