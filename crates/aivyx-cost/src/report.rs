//! `CostReport` — the aggregation over a turn's-worth of usage (Chapter K.2).
//!
//! The audit chain holds one `AuditEvent::LlmCost { model, usage }` per
//! LLM-backed turn. The daemon maps those to `(model, TokenCounts)` pairs and
//! feeds them here; this module is **pure** — it rolls them up by model and in
//! total (turns and tokens), pricing each pair via a [`Pricing`] table. Local
//! models cost `$0`, so `usd` is only non-zero when the operator set a
//! `[pricing.<model>]` rate. Period scoping (today / this run / this session)
//! is the *caller's* job: filter the entries to the window, then build a
//! report over them.

use std::collections::BTreeMap;

use crate::{Pricing, TokenCounts};

/// One model's rolled-up line in a [`CostReport`].
#[derive(Debug, Clone, PartialEq)]
pub struct ModelLine {
    pub model: String,
    /// LLM-backed turns attributed to this model.
    pub turns: u64,
    pub tokens: TokenCounts,
    /// Spend (USD) from an operator rate; `0.0` without one.
    pub usd: f64,
}

/// A roll-up of LLM usage over some set of turns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostReport {
    /// Per-model lines, keyed (and iterated) by model name.
    pub by_model: BTreeMap<String, ModelLine>,
    /// Total LLM-backed turns in the report.
    pub turns: u64,
    /// Total tokens across every model + class.
    pub tokens: TokenCounts,
    /// Total spend (USD) from operator rates; `0.0` without any.
    pub usd: f64,
}

impl CostReport {
    /// Aggregate `entries` (each a `(model, usage)` pair, e.g. mapped from
    /// `LlmCost` chain events), pricing them against `pricing`.
    pub fn build<'a, I>(entries: I, pricing: &Pricing) -> Self
    where
        I: IntoIterator<Item = (&'a str, TokenCounts)>,
    {
        let mut report = CostReport::default();
        for (model, tokens) in entries {
            let cost = pricing.cost_of(model, &tokens);
            let line = report
                .by_model
                .entry(model.to_string())
                .or_insert_with(|| ModelLine {
                    model: model.to_string(),
                    turns: 0,
                    tokens: TokenCounts::default(),
                    usd: 0.0,
                });
            line.turns += 1;
            line.tokens.add(&tokens);
            line.usd += cost.usd;

            report.turns += 1;
            report.tokens.add(&tokens);
            report.usd += cost.usd;
        }
        report
    }

    /// No turns recorded.
    pub fn is_empty(&self) -> bool {
        self.turns == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelRate;

    fn io(input: u64, output: u64) -> TokenCounts {
        TokenCounts::new(input, output)
    }

    fn entries() -> Vec<(&'static str, TokenCounts)> {
        vec![
            ("qwen3.8-27b", io(1_000_000, 1_000_000)),
            ("qwen3.8-27b", io(1_000_000, 0)),
            ("llama3.1", io(5_000_000, 5_000_000)),
            ("mystery-model", io(1_000_000, 1_000_000)),
        ]
    }

    #[test]
    fn counts_turns_and_tokens_at_zero_cost() {
        let r = CostReport::build(entries(), &Pricing::new());
        assert_eq!(r.turns, 4);
        assert_eq!(r.tokens.input, 8_000_000);
        assert_eq!(r.tokens.output, 7_000_000);
        assert_eq!(r.usd, 0.0, "local runs cost nothing");
        assert!(r.by_model.values().all(|l| l.usd == 0.0));
    }

    #[test]
    fn aggregates_repeated_models() {
        let r = CostReport::build(entries(), &Pricing::new());
        let q = &r.by_model["qwen3.8-27b"];
        assert_eq!(q.turns, 2);
        assert_eq!(q.tokens.input, 2_000_000);
        assert_eq!(q.tokens.output, 1_000_000);
    }

    #[test]
    fn an_operator_rate_is_applied() {
        let p = Pricing::with_overrides([("llama3.1".to_string(), ModelRate::io(1.0, 1.0))]);
        let r = CostReport::build(entries(), &p);
        assert!((r.by_model["llama3.1"].usd - 10.0).abs() < 1e-9);
        assert!((r.usd - 10.0).abs() < 1e-9);
    }

    #[test]
    fn empty_report() {
        let r = CostReport::build(Vec::<(&str, TokenCounts)>::new(), &Pricing::new());
        assert!(r.is_empty());
        assert_eq!(r.usd, 0.0);
    }
}
