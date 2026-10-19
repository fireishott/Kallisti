//! Token accounting.
//!
//! The rule the whole type exists for: **usage a provider did not report is
//! unavailable, never zero.** A provider that cannot say how many prompt tokens
//! came from its cache produces [`CacheRead::ProviderUnavailable`], and one that
//! could not report usage at all produces [`TokenCount::ProviderUnavailable`].
//! Neither can be constructed as a zero by accident, because a zero is a
//! different value ([`CacheRead::Reported`] with `tokens: 0`) with a different
//! meaning: the provider looked, and found none.
//!
//! The second rule: when a provider folds cache reads into its prompt total -
//! OpenAI's `prompt_tokens` includes `prompt_tokens_details.cached_tokens` -
//! [`Usage::reconcile`] subtracts them, so a folded provider and a
//! separate-field provider (Anthropic's `cache_read_input_tokens` sits beside an
//! `input_tokens` that already excludes it) agree on [`Usage::input_tokens`].
//! The billable input count does not depend on the gateway's bookkeeping.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::profile::CacheReadReporting;

/// Why a usage number is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// The provider never sent the fields at all.
    FieldsAbsent,
    /// The deployment's capabilities say this provider cannot report them, so
    /// any value it did send would be noise.
    NotSupportedByProvider,
    /// The stream ended before a usage frame arrived.
    StreamEndedBeforeUsage,
}

impl UnavailableReason {
    /// Stable wire name, matching the serde tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FieldsAbsent => "fields_absent",
            Self::NotSupportedByProvider => "not_supported_by_provider",
            Self::StreamEndedBeforeUsage => "stream_ended_before_usage",
        }
    }
}

/// A token count that may be unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TokenCount {
    /// The provider reported this many.
    Reported {
        /// Token count as reported.
        tokens: u64,
    },
    /// The provider did not report it. Never conflated with zero.
    ProviderUnavailable {
        /// Why it is unknown.
        reason: UnavailableReason,
    },
}

impl TokenCount {
    /// A reported count.
    #[must_use]
    pub const fn reported(tokens: u64) -> Self {
        Self::Reported { tokens }
    }

    /// An unknown count.
    #[must_use]
    pub const fn unavailable(reason: UnavailableReason) -> Self {
        Self::ProviderUnavailable { reason }
    }

    /// The count, when it was reported.
    #[must_use]
    pub const fn tokens(self) -> Option<u64> {
        match self {
            Self::Reported { tokens } => Some(tokens),
            Self::ProviderUnavailable { .. } => None,
        }
    }

    /// Whether the provider failed to report this.
    #[must_use]
    pub const fn is_unavailable(self) -> bool {
        matches!(self, Self::ProviderUnavailable { .. })
    }
}

impl fmt::Display for TokenCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reported { tokens } => write!(f, "{tokens}"),
            Self::ProviderUnavailable { .. } => f.write_str("unavailable"),
        }
    }
}

/// Prompt tokens the provider served from its cache.
///
/// [`CacheRead::ProviderUnavailable`] and `Reported { tokens: 0, .. }` are
/// different facts and serialize differently, which is the point: "this provider
/// cannot tell me" must never render as "nothing was cached".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CacheRead {
    /// The provider reported cache reads.
    Reported {
        /// Cached prompt tokens as reported.
        tokens: u64,
        /// Whether those tokens are also included in the prompt total. Used to
        /// decide the subtraction that produced [`Usage::input_tokens`]; kept on
        /// the value so a stored usage record explains its own arithmetic.
        folded_into_prompt: bool,
    },
    /// The provider did not report cache reads. Never zero.
    ProviderUnavailable {
        /// Why it is unknown.
        reason: UnavailableReason,
    },
}

impl CacheRead {
    /// The cached token count, when it was reported.
    #[must_use]
    pub const fn tokens(self) -> Option<u64> {
        match self {
            Self::Reported { tokens, .. } => Some(tokens),
            Self::ProviderUnavailable { .. } => None,
        }
    }

    /// Whether the provider failed to report cache reads.
    #[must_use]
    pub const fn is_unavailable(self) -> bool {
        matches!(self, Self::ProviderUnavailable { .. })
    }

    /// The reason, when it is unknown.
    #[must_use]
    pub const fn unavailable_reason(self) -> Option<UnavailableReason> {
        match self {
            Self::ProviderUnavailable { reason } => Some(reason),
            Self::Reported { .. } => None,
        }
    }
}

impl fmt::Display for CacheRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reported { tokens, .. } => write!(f, "{tokens}"),
            Self::ProviderUnavailable { .. } => f.write_str("unavailable"),
        }
    }
}

/// One turn's token accounting.
///
/// Construct through [`Usage::reconcile`] or [`Usage::unavailable`]; the fields
/// are private so nothing can assemble a zero for a number it does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    input: TokenCount,
    output: TokenCount,
    cache_read: CacheRead,
}

impl Usage {
    /// Build usage from what a provider actually sent.
    ///
    /// - `reporting` is the deployment's declared capability, which decides
    ///   whether a cache count is subtracted from the prompt total and whether
    ///   a cache count is believed at all;
    /// - `prompt_tokens` is the provider's prompt total, if it sent one;
    /// - `cache_read_tokens` is its cache count, if it sent one;
    /// - `output_tokens` is its completion count, if it sent one.
    ///
    /// A missing cache count, or a provider capability of
    /// [`CacheReadReporting::NotReported`], yields
    /// [`CacheRead::ProviderUnavailable`]. It cannot yield zero.
    #[must_use]
    pub fn reconcile(
        reporting: CacheReadReporting,
        prompt_tokens: Option<u64>,
        cache_read_tokens: Option<u64>,
        output_tokens: Option<u64>,
    ) -> Self {
        let cache_read = match (reporting, cache_read_tokens) {
            (CacheReadReporting::NotReported, _) => CacheRead::ProviderUnavailable {
                reason: UnavailableReason::NotSupportedByProvider,
            },
            (_, None) => CacheRead::ProviderUnavailable {
                reason: UnavailableReason::FieldsAbsent,
            },
            (CacheReadReporting::FoldedIntoPrompt, Some(tokens)) => CacheRead::Reported {
                tokens,
                folded_into_prompt: true,
            },
            (CacheReadReporting::Separate, Some(tokens)) => CacheRead::Reported {
                tokens,
                folded_into_prompt: false,
            },
        };

        let input = match prompt_tokens {
            None => TokenCount::unavailable(UnavailableReason::FieldsAbsent),
            Some(total) => {
                // Only a folded count is inside the total. A separate one already
                // sits outside it, and an unknown one means the total is all we
                // have - so it is reported as the input, with the cache state
                // saying we cannot split it.
                let uncached = match cache_read {
                    CacheRead::Reported {
                        tokens,
                        folded_into_prompt: true,
                    } => total.saturating_sub(tokens),
                    _ => total,
                };
                TokenCount::reported(uncached)
            }
        };

        let output = match output_tokens {
            Some(tokens) => TokenCount::reported(tokens),
            None => TokenCount::unavailable(UnavailableReason::FieldsAbsent),
        };

        Self {
            input,
            output,
            cache_read,
        }
    }

    /// Usage for a turn where the provider reported nothing at all.
    #[must_use]
    pub const fn unavailable(reason: UnavailableReason) -> Self {
        Self {
            input: TokenCount::unavailable(reason),
            output: TokenCount::unavailable(reason),
            cache_read: CacheRead::ProviderUnavailable { reason },
        }
    }

    /// Billable input tokens: the prompt total minus cache reads when the
    /// provider folded them in.
    #[must_use]
    pub const fn input_tokens(&self) -> Option<u64> {
        self.input.tokens()
    }

    /// Output tokens as reported.
    #[must_use]
    pub const fn output_tokens(&self) -> Option<u64> {
        self.output.tokens()
    }

    /// Cache read tokens as reported, or `None` when the provider could not say.
    ///
    /// `None` here is not zero. Use [`Usage::cache_read`] when the reason
    /// matters.
    #[must_use]
    pub const fn cache_read_tokens(&self) -> Option<u64> {
        self.cache_read.tokens()
    }

    /// The full cache-read fact, including why it is unknown.
    #[must_use]
    pub const fn cache_read(&self) -> CacheRead {
        self.cache_read
    }

    /// The input fact, including why it is unknown.
    #[must_use]
    pub const fn input(&self) -> TokenCount {
        self.input
    }

    /// The output fact, including why it is unknown.
    #[must_use]
    pub const fn output(&self) -> TokenCount {
        self.output
    }

    /// Input plus output, when both were reported.
    ///
    /// Cache reads are already excluded from the input when the provider folded
    /// them in, so they are never added back here.
    #[must_use]
    pub const fn total_tokens(&self) -> Option<u64> {
        match (self.input.tokens(), self.output.tokens()) {
            (Some(input), Some(output)) => Some(input + output),
            _ => None,
        }
    }
}

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "usage: input={} output={} cache_read={}",
            self.input, self.output, self.cache_read
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folded_cache_read_is_subtracted_from_the_prompt_total() {
        // OpenAI shape: 100 prompt tokens of which 40 were cache hits.
        let usage = Usage::reconcile(
            CacheReadReporting::FoldedIntoPrompt,
            Some(100),
            Some(40),
            Some(10),
        );
        assert_eq!(usage.input_tokens(), Some(60));
        assert_eq!(usage.output_tokens(), Some(10));
        assert_eq!(usage.cache_read_tokens(), Some(40));
        assert_eq!(usage.total_tokens(), Some(70));
        assert_eq!(
            usage.cache_read(),
            CacheRead::Reported {
                tokens: 40,
                folded_into_prompt: true
            }
        );
    }

    #[test]
    fn a_separate_cache_read_leaves_the_prompt_total_alone() {
        // Anthropic shape: input_tokens already excludes the cache read.
        let usage = Usage::reconcile(CacheReadReporting::Separate, Some(60), Some(40), Some(10));
        assert_eq!(usage.input_tokens(), Some(60));
        assert_eq!(usage.cache_read_tokens(), Some(40));
        assert_eq!(
            usage.cache_read(),
            CacheRead::Reported {
                tokens: 40,
                folded_into_prompt: false
            }
        );
    }

    #[test]
    fn an_absent_cache_read_is_unavailable_never_zero() {
        let usage = Usage::reconcile(
            CacheReadReporting::FoldedIntoPrompt,
            Some(100),
            None,
            Some(10),
        );
        assert_eq!(usage.cache_read_tokens(), None);
        assert_ne!(usage.cache_read_tokens(), Some(0));
        assert!(usage.cache_read().is_unavailable());
        assert_eq!(
            usage.cache_read().unavailable_reason(),
            Some(UnavailableReason::FieldsAbsent)
        );
        // The prompt total is still the best available input count.
        assert_eq!(usage.input_tokens(), Some(100));
        // And it renders as unavailable, not as zero.
        assert_eq!(
            usage.to_string(),
            "usage: input=100 output=10 cache_read=unavailable"
        );
    }

    #[test]
    fn a_reported_zero_cache_read_is_a_different_value_from_an_absent_one() {
        let reported = Usage::reconcile(
            CacheReadReporting::FoldedIntoPrompt,
            Some(100),
            Some(0),
            Some(10),
        );
        let absent = Usage::reconcile(
            CacheReadReporting::FoldedIntoPrompt,
            Some(100),
            None,
            Some(10),
        );

        assert_eq!(reported.cache_read_tokens(), Some(0));
        assert_eq!(absent.cache_read_tokens(), None);
        assert_ne!(reported.cache_read(), absent.cache_read());
        assert!(reported.to_string().contains("cache_read=0"));
        assert!(absent.to_string().contains("cache_read=unavailable"));
    }

    #[test]
    fn a_provider_that_cannot_report_cache_reads_ignores_a_sent_value() {
        let usage = Usage::reconcile(
            CacheReadReporting::NotReported,
            Some(100),
            Some(40),
            Some(5),
        );
        assert_eq!(usage.cache_read_tokens(), None);
        assert_eq!(
            usage.cache_read().unavailable_reason(),
            Some(UnavailableReason::NotSupportedByProvider)
        );
        // Nothing was subtracted, because nothing was trustworthy.
        assert_eq!(usage.input_tokens(), Some(100));
    }

    #[test]
    fn usage_can_be_unavailable_in_every_field() {
        let usage = Usage::unavailable(UnavailableReason::StreamEndedBeforeUsage);
        assert_eq!(
            usage.to_string(),
            "usage: input=unavailable output=unavailable cache_read=unavailable"
        );
        assert_eq!(usage.total_tokens(), None);
        assert!(usage.input().is_unavailable() && usage.output().is_unavailable());
    }

    #[test]
    fn a_cache_count_larger_than_the_prompt_total_saturates() {
        // A provider bug, not a panic: the billable side cannot go negative.
        let usage = Usage::reconcile(
            CacheReadReporting::FoldedIntoPrompt,
            Some(10),
            Some(40),
            Some(1),
        );
        assert_eq!(usage.input_tokens(), Some(0));
        assert_eq!(usage.cache_read_tokens(), Some(40));
    }

    #[test]
    fn usage_round_trips_through_json_with_its_unavailable_state() {
        let absent = Usage::reconcile(
            CacheReadReporting::FoldedIntoPrompt,
            Some(100),
            None,
            Some(1),
        );
        let json = serde_json::to_string(&absent).expect("usage should serialize");
        assert!(
            json.contains("\"state\":\"provider_unavailable\""),
            "{json}"
        );
        assert!(json.contains("fields_absent"), "{json}");
        let back: Usage = serde_json::from_str(&json).expect("usage should deserialize");
        assert_eq!(back, absent);
    }
}
