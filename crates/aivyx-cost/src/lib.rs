//! `aivyx-cost` — cost governance (Chapter K).
//!
//! Free-core **visibility + caps** over LLM usage (not customer billing). The
//! pricing primitive turns a [`TokenCounts`] record + a model name into a
//! [`Cost`], via a [`Pricing`] table of operator rates.
//!
//! aivyx is local-only (2.0): every model runs on the operator's own machine
//! or network, so **every model costs `$0`** unless the operator sets an
//! explicit `[pricing.<model>]` rate (for example to account for electricity
//! or a shared GPU box). There are no shipped price tables. Reports show
//! tokens and runs; the dollar column only appears when an operator rate is
//! set.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub mod budget;
pub mod rate_limit;
pub mod report;
pub use budget::{
    BudgetAction, BudgetConfig, BudgetEnforcer, BudgetVerdict, MissionBudget, Reservation,
};
pub use rate_limit::{RateAction, RateLimitConfig, RateLimiter, RateVerdict, ToolRateConfig};
pub use report::{CostReport, ModelLine};

/// Token counts for one priced unit — a single turn, or a whole run summed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    /// Cached input tokens read back (cheaper than fresh input).
    pub cache_read: u64,
    /// Tokens written into the prompt cache (a premium over fresh input).
    pub cache_write: u64,
}

impl TokenCounts {
    /// Plain input/output counts (no prompt caching).
    pub fn new(input: u64, output: u64) -> Self {
        TokenCounts {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
        }
    }

    /// Total tokens across all classes — the model-agnostic volume figure the
    /// loop's existing token cap uses.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// Accumulate another record (for run/day aggregation in K.2).
    pub fn add(&mut self, other: &TokenCounts) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

/// A model's price, in **USD per million tokens**, by token class.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelRate {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

impl ModelRate {
    /// A rate with only input/output set (cache classes default to 0).
    pub const fn io(input: f64, output: f64) -> Self {
        ModelRate {
            input,
            output,
            cache_read: 0.0,
            cache_write: 0.0,
        }
    }

    /// Builder: set the cache read/write rates.
    pub const fn with_cache(mut self, read: f64, write: f64) -> Self {
        self.cache_read = read;
        self.cache_write = write;
        self
    }

    /// Apply this rate to a usage record → dollars.
    fn apply(&self, t: &TokenCounts) -> f64 {
        const PER_MTOK: f64 = 1_000_000.0;
        (t.input as f64 * self.input
            + t.output as f64 * self.output
            + t.cache_read as f64 * self.cache_read
            + t.cache_write as f64 * self.cache_write)
            / PER_MTOK
    }
}

/// The result of pricing a usage record.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    /// Dollars — `0.0` unless the operator set a rate for the model.
    pub usd: f64,
}

/// A model → [`ModelRate`] table of exact-match operator rates (from
/// `[pricing.<model>]` config). A model with no rate costs `$0`.
#[derive(Debug, Clone, Default)]
pub struct Pricing {
    /// Exact-model operator rates.
    overrides: HashMap<String, ModelRate>,
}

impl Pricing {
    /// An empty pricing table: every model costs `$0`.
    pub fn new() -> Self {
        Pricing::default()
    }

    /// A table seeded with exact-model operator rates (from
    /// `[pricing.<model>]` config).
    pub fn with_overrides(overrides: impl IntoIterator<Item = (String, ModelRate)>) -> Self {
        Pricing {
            overrides: overrides.into_iter().collect(),
        }
    }

    /// Set (or replace) the exact rate for a model.
    pub fn set_rate(&mut self, model: impl Into<String>, rate: ModelRate) {
        self.overrides.insert(model.into(), rate);
    }

    /// The operator rate for `model`, if one is set.
    pub fn rate(&self, model: &str) -> Option<ModelRate> {
        self.overrides.get(model).copied()
    }

    /// Price a usage record for `model`: the operator rate if one is set,
    /// otherwise `$0` (local inference).
    pub fn cost_of(&self, model: &str, tokens: &TokenCounts) -> Cost {
        Cost {
            usd: self.overrides.get(model).map_or(0.0, |r| r.apply(tokens)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(input: u64, output: u64) -> TokenCounts {
        TokenCounts::new(input, output)
    }

    #[test]
    fn token_counts_total_and_add() {
        let mut a = TokenCounts {
            input: 100,
            output: 50,
            cache_read: 10,
            cache_write: 5,
        };
        assert_eq!(a.total(), 165);
        a.add(&TokenCounts::new(1, 2));
        assert_eq!(a.input, 101);
        assert_eq!(a.output, 52);
    }

    #[test]
    fn every_model_is_free_without_an_operator_rate() {
        let p = Pricing::new();
        for model in ["llama3.1", "qwen3:27b", "claude-sonnet-4-6", "gpt-4o", "mystery-model"] {
            let c = p.cost_of(model, &counts(5_000_000, 5_000_000));
            assert_eq!(c.usd, 0.0, "{model} costs nothing without a rate");
        }
        assert!(p.rate("claude-opus-4-8").is_none(), "no shipped price tables");
    }

    #[test]
    fn with_overrides_seeds_the_table() {
        let p = Pricing::with_overrides([("llama-box".to_string(), ModelRate::io(1.0, 2.0))]);
        // 1M in + 1M out → $1 + $2.
        let c = p.cost_of("llama-box", &counts(1_000_000, 1_000_000));
        assert!((c.usd - 3.0).abs() < 1e-9);
        assert_eq!(p.cost_of("other", &counts(1_000_000, 0)).usd, 0.0);
    }

    #[test]
    fn set_rate_prices_cache_classes() {
        let mut p = Pricing::new();
        p.set_rate("qwen", ModelRate::io(0.0, 0.0).with_cache(0.3, 3.75));
        let u = TokenCounts {
            input: 0,
            output: 0,
            cache_read: 1_000_000,
            cache_write: 1_000_000,
        };
        assert!((p.cost_of("qwen", &u).usd - 4.05).abs() < 1e-9);
        assert!(p.rate("qwen").is_some());
    }

    #[test]
    fn zero_usage_is_zero_cost() {
        let mut p = Pricing::new();
        p.set_rate("m", ModelRate::io(9.0, 9.0));
        assert_eq!(p.cost_of("m", &TokenCounts::default()).usd, 0.0);
    }

    #[test]
    fn model_rate_serde_roundtrip_defaults_cache_to_zero() {
        // Config may give only input/output; cache classes default to 0.
        let r: ModelRate = serde_json::from_str(r#"{"input":3.0,"output":15.0}"#).unwrap();
        assert_eq!(r.cache_read, 0.0);
        assert_eq!(r.cache_write, 0.0);
        let back = serde_json::to_string(&r).unwrap();
        let r2: ModelRate = serde_json::from_str(&back).unwrap();
        assert_eq!(r, r2);
    }
}
