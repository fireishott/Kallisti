//! What a turn cost, in dollars, from the prices an operator declares.
//!
//! The daemon reports tokens because a provider states them. It reports a cost
//! only when somebody has said what a token costs, and the shipped table is
//! empty on purpose: a price is a fact about a contract, not about a model, and
//! a daemon that guessed one would put a confident dollar figure under every
//! answer and be wrong on every plan that bills differently.
//!
//! So there are two ways a price arrives, and both are explicit:
//!
//! - the operator writes it into `settings` under [`SETTING_PRICES`], which is
//!   what the dashboard's price editor does; or
//! - a model is declared local ([`LOCAL_MARKERS`]), which is a price of zero
//!   the operator did not have to state.
//!
//! Everything else is [`Cost::Unknown`], and the caller omits the field. That is
//! the same rule the token counts follow: a number the daemon did not measure is
//! absent, never zero.

use serde::{Deserialize, Serialize};

/// The `settings` row carrying the operator's price table, as JSON.
pub const SETTING_PRICES: &str = "assistant.model_prices";

/// A model id fragment that marks a model as running on this host.
///
/// A local model costs nothing per token, which is a real price and not a
/// missing one. It still needs saying explicitly: this is an allowlist, not a
/// prefix match against every id that mentions "local".
pub const LOCAL_MARKERS: &[&str] = &["ollama/", "lmstudio/", "local/", ":11434/"];

/// What one million tokens of a model cost, in US dollars.
///
/// Every field is optional because a provider may price input and output
/// separately and say nothing about cache reads. Absent is not zero: a model
/// with `output: None` cannot answer "what did this cost", and the caller says
/// so rather than charging zero for the completion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    /// Dollars per million prompt tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    /// Dollars per million completion tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    /// Dollars per million prompt tokens served from cache.
    ///
    /// When absent the input rate is used, which is what a provider that folds
    /// cache reads into its prompt total charges. A provider with a cheaper
    /// cache rate needs this stated, or the estimate reads high.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<f64>,
}

impl Price {
    /// Whether this price can value a turn at all.
    ///
    /// Input and output are both required: a turn has both, so a table that
    /// names only one of them cannot produce a total.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.input.is_some() && self.output.is_some()
    }
}

/// What one turn cost, or why that cannot be said.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cost {
    /// The turn's dollar cost, in US dollars.
    Amount(f64),
    /// The model is not in any price table, so there is no number to state.
    Unpriced,
}

/// Read the operator's price table out of its `settings` row.
///
/// A malformed row is an empty table rather than an error: a price table that
/// cannot be parsed must not stop a turn from settling, and the consequence of
/// ignoring it is that the footer omits a cost, which is the same outcome as
/// having declared nothing.
#[must_use]
pub fn parse_prices(stored: Option<&str>) -> std::collections::BTreeMap<String, Price> {
    let Some(raw) = stored.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return std::collections::BTreeMap::new();
    };
    serde_json::from_str::<std::collections::BTreeMap<String, Price>>(raw).unwrap_or_default()
}

/// Whether a model id names something that runs on this host.
///
/// Matched against the id, provider-qualified as it arrives: `ds/deepseek-v4-flash`
/// is remote, `ollama/qwen2.5:7b` is not.
#[must_use]
pub fn is_local(model: &str) -> bool {
    let lowered = model.trim().to_lowercase();
    LOCAL_MARKERS.iter().any(|marker| lowered.starts_with(marker))
}

/// The published price for one model, when there is one.
///
/// Lookup is exact first, then the id's own bare name. `ds/deepseek-v4-flash`
/// and `deepseek-v4-flash` are the same model under two spellings - one
/// provider-qualified, one not - and an operator writes the price table by hand,
/// so both have to resolve or the table only works from the surface it was
/// typed on.
#[must_use]
pub fn price_for(
    model: &str,
    table: &std::collections::BTreeMap<String, Price>,
) -> Option<Price> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(found) = table.get(trimmed) {
        return Some(*found);
    }
    let bare = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if bare != trimmed
        && let Some(found) = table.get(bare)
    {
        return Some(*found);
    }
    // The other direction: a row written with a provider prefix and a caller
    // that named only the model. Compared on the bare name so `ag/gemini-3-flash`
    // matches a `gemini-3-flash` row.
    table
        .iter()
        .find(|(key, _)| key.rsplit('/').next() == Some(bare))
        .map(|(_, price)| *price)
}

/// What one turn cost, given the tokens it reported.
///
/// Returns [`Cost::Unpriced`] whenever any required number is missing - the
/// model has no complete price, or a component was never reported. A partial
/// sum reported as a total would be a wrong number presented as a measurement,
/// which is worse than the footer saying nothing.
///
/// The cached share is taken out of the prompt first. It has already been
/// subtracted from `prompt_tokens` by the time this sees it for a folded
/// provider, and sits beside it for a separate one; either way, multiplying the
/// whole prompt by the input rate would charge full price for tokens the
/// provider billed at the cache rate. `cached` is therefore charged once, at
/// [`Price::cache`], and `prompt - cached` at [`Price::input`].
#[must_use]
pub fn cost_of(
    model: &str,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    table: &std::collections::BTreeMap<String, Price>,
) -> Cost {
    if is_local(model) {
        // A local model is free whether or not a row exists. Charging the input
        // rate for something running on this box would be a fabricated cost.
        return Cost::Amount(0.0);
    }
    let Some(price) = price_for(model, table) else {
        return Cost::Unpriced;
    };
    if !price.is_complete() {
        return Cost::Unpriced;
    }
    let (Some(prompt), Some(completion)) = (prompt_tokens, completion_tokens) else {
        return Cost::Unpriced;
    };
    let input_rate = price.input.unwrap_or_default();
    let output_rate = price.output.unwrap_or_default();
    let cache_rate = price.cache.unwrap_or(input_rate);
    // The cached count is clamped to the prompt: a provider that reports cache
    // reads outside its prompt total (Anthropic does) would otherwise make the
    // uncached share negative and undercharge the turn.
    let cached = cached_tokens.unwrap_or(0).min(prompt);
    let uncached = prompt.saturating_sub(cached);
    #[allow(clippy::cast_precision_loss)]
    let total = (uncached as f64 / 1_000_000.0) * input_rate
        + (cached as f64 / 1_000_000.0) * cache_rate
        + (completion as f64 / 1_000_000.0) * output_rate;
    // A price of zero everywhere is a real answer (`free`), so only a total that
    // is not a number is refused.
    if total.is_finite() {
        Cost::Amount(total.max(0.0))
    } else {
        Cost::Unpriced
    }
}

/// A cost as the footer draws it.
///
/// Four decimals below a cent, because a single turn costs a fraction of one and
/// `$0.00` under every answer reads like the meter is broken. `<$0.0001` marks a
/// real but tiny charge that four decimals would round to nothing.
#[must_use]
pub fn money(amount: f64) -> String {
    if amount <= 0.0 {
        "free".to_owned()
    } else if amount < 0.0001 {
        "<$0.0001".to_owned()
    } else {
        format!("~${amount:.4}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn table(entries: &[(&str, Option<f64>, Option<f64>, Option<f64>)]) -> BTreeMap<String, Price> {
        entries
            .iter()
            .map(|(name, input, output, cache)| {
                (
                    (*name).to_owned(),
                    Price {
                        input: *input,
                        output: *output,
                        cache: *cache,
                    },
                )
            })
            .collect()
    }

    /// The shipped state is no table at all, and an empty table prices nothing.
    #[test]
    fn with_no_prices_nothing_is_priced() {
        let empty = parse_prices(None);
        assert!(empty.is_empty());
        assert_eq!(
            cost_of("deepseek/deepseek-v4-flash", Some(10), Some(5), None, &empty),
            Cost::Unpriced
        );
        assert!(parse_prices(Some("{not json")).is_empty());
    }

    /// Input and output are both required: a turn has both.
    #[test]
    fn a_half_written_price_cannot_value_a_turn() {
        let table = table(&[("m", Some(1.0), None, None)]);
        assert!(!table["m"].is_complete());
        assert_eq!(cost_of("m", Some(1_000), Some(500), None, &table), Cost::Unpriced);
    }

    /// One million prompt and one million completion tokens at a $1/$2 table is
    /// $3, exactly, with no cached share.
    #[test]
    fn a_plain_turn_is_input_plus_output() {
        let table = table(&[("m", Some(1.0), Some(2.0), None)]);
        assert_eq!(
            cost_of("m", Some(1_000_000), Some(1_000_000), None, &table),
            Cost::Amount(3.0)
        );
    }

    /// The cached share is charged once, at the cache rate, and not again at the
    /// input rate on top.
    #[test]
    fn a_cached_share_is_charged_at_its_own_rate() {
        let table = table(&[("m", Some(1.0), Some(2.0), Some(0.1))]);
        // 900k uncached at $1 + 100k cached at $0.10 + 0 output = $0.91.
        let cost = cost_of("m", Some(1_000_000), Some(0), Some(100_000), &table);
        match cost {
            Cost::Amount(amount) => assert!((amount - 0.91).abs() < 1e-9, "{amount}"),
            Cost::Unpriced => panic!("a complete table prices a complete turn"),
        }
    }

    /// A cache count larger than the prompt is clamped, not subtracted into a
    /// negative charge.
    #[test]
    fn a_cache_count_above_the_prompt_does_not_undercharge() {
        let table = table(&[("m", Some(1.0), Some(0.0), Some(0.0))]);
        // 1k prompt, 5k claimed cached: the whole prompt is cached, not -4k.
        match cost_of("m", Some(1_000), Some(0), Some(5_000), &table) {
            Cost::Amount(amount) => assert_eq!(amount, 0.0),
            Cost::Unpriced => panic!("priced"),
        }
    }

    /// A local model is free and an unpriced remote one is not.
    #[test]
    fn a_local_model_costs_nothing_and_an_unknown_one_costs_nothing_said() {
        let empty = parse_prices(None);
        assert_eq!(
            cost_of("ollama/qwen2.5:7b", Some(1_000), Some(100), None, &empty),
            Cost::Amount(0.0)
        );
        assert_eq!(
            cost_of("ds/deepseek-v4-flash", Some(1_000), Some(100), None, &empty),
            Cost::Unpriced
        );
    }

    /// A price row resolves through the provider prefix in either direction, so
    /// a hand-written table does not only work from the surface it was typed on.
    #[test]
    fn a_price_finds_its_model_with_or_without_the_prefix() {
        let with_prefix = table(&[("ds/deepseek-v4-flash", Some(1.0), Some(2.0), None)]);
        assert_eq!(
            cost_of("deepseek-v4-flash", Some(1_000_000), Some(0), None, &with_prefix),
            Cost::Amount(1.0)
        );
        let without = table(&[("deepseek-v4-flash", Some(1.0), Some(2.0), None)]);
        assert_eq!(
            cost_of("ds/deepseek-v4-flash", Some(1_000_000), Some(0), None, &without),
            Cost::Amount(1.0)
        );
    }

    /// The rendering, including the two ends: a real zero and a real crumb.
    #[test]
    fn money_reads_free_a_crumb_and_a_charge() {
        assert_eq!(money(0.0), "free");
        assert_eq!(money(-1.0), "free");
        assert_eq!(money(0.00001), "<$0.0001");
        assert_eq!(money(0.0084), "~$0.0084");
    }
}
