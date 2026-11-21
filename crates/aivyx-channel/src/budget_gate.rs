//! Chapter K (K.4.2) — the concrete pre-call dollar gate for the turn loop.
//!
//! [`aivyx_core::BudgetGate`] is a thin trait so `ConcreteAgent` stays
//! ignorant of pricing and the audit chain. This module supplies the real
//! implementation: at the start of each LLM-backed turn it
//!
//! 1. sums **committed** spend over the last 24h from the HMAC chain's
//!    `LlmCost` events (the same rolling window `aivyx-pa cost --today` uses),
//! 2. prices a conservative **estimate** for the upcoming turn (the planner's
//!    whole `max_tokens` output budget at the model's rate), and
//! 3. **reserves** that estimate against a [`BudgetEnforcer`], which checks
//!    `committed + reserved + estimate` against the operator's `per_day_usd`
//!    cap and denies the turn if it would bust a `Deny` cap.
//!
//! The returned guard holds the reservation for the turn's lifetime; its
//! `Drop` releases it (by then the actual cost is already an `LlmCost` event
//! on the chain, so the next turn's committed figure reflects it).
//!
//! **Scope:** this gate enforces the **per-day** cap only. The per-run cap is
//! the autonomous loop driver's job (K.4.3) — "run" is undefined for an
//! interactive turn — so the enforcer is built with `per_run_usd: None` and
//! `reserve` is called with `run_usd = 0.0`. Reservations still close the
//! team-concurrency TOCTOU on the day cap.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aivyx_audit::{AuditEvent, PersistentAuditLog};
use aivyx_core::{BudgetGate, TokenUsage, TurnBudgetGuard};
use aivyx_cost::{
    BudgetConfig, BudgetEnforcer, Pricing, Reservation, TokenCounts,
};

const PAGE_SIZE: usize = 1024;
const DAY: Duration = Duration::from_secs(24 * 3600);

/// Map an audit `TokenUsage` to the cost crate's `TokenCounts`
/// (cache_creation ⇒ cache_write, cache_read ⇒ cache_read). Mirrors the
/// `aivyx-pa cost` mapping so committed spend is priced identically.
fn to_counts(u: &TokenUsage) -> TokenCounts {
    TokenCounts {
        input: u.input_tokens as u64,
        output: u.output_tokens as u64,
        cache_read: u.cache_read_input_tokens as u64,
        cache_write: u.cache_creation_input_tokens as u64,
    }
}

/// The concrete [`BudgetGate`]: a [`BudgetEnforcer`] over the day cap, plus the
/// chain handle + pricing it needs to compute committed spend and estimates.
pub struct ChannelBudgetGate {
    enforcer: Arc<BudgetEnforcer>,
    audit_log: Arc<PersistentAuditLog>,
    pricing: Pricing,
    /// The planner's per-turn max output tokens — the estimate's basis.
    max_tokens: u32,
}

impl ChannelBudgetGate {
    /// Build a gate from the operator's `[budget]` config. The enforcer is
    /// constructed with `per_run_usd` forced to `None`: the turn-loop gate is
    /// day-scoped (see the module docs). Returns `None` when no day cap is
    /// set, so callers can skip attaching a gate entirely (zero overhead).
    pub fn new(
        budget: BudgetConfig,
        audit_log: Arc<PersistentAuditLog>,
        pricing: Pricing,
        max_tokens: u32,
    ) -> Option<Self> {
        budget.per_day_usd?;
        let day_only = BudgetConfig {
            per_run_usd: None,
            ..budget
        };
        Some(ChannelBudgetGate {
            enforcer: Arc::new(BudgetEnforcer::new(day_only)),
            audit_log,
            pricing,
            max_tokens,
        })
    }

    /// Convenience for agent-construction sites: build the gate already boxed
    /// as the `aivyx_core::BudgetGate` trait object, or `None` when no day cap
    /// is set (so callers attach nothing and keep ungated behavior).
    pub fn new_gate(
        budget: BudgetConfig,
        audit_log: Arc<PersistentAuditLog>,
        pricing: Pricing,
        max_tokens: u32,
    ) -> Option<Arc<dyn BudgetGate>> {
        Self::new(budget, audit_log, pricing, max_tokens)
            .map(|g| Arc::new(g) as Arc<dyn BudgetGate>)
    }

    /// Sum priced `LlmCost` spend over the last 24h from the chain. On a read
    /// error this degrades to `0.0` (fail-open) with a warning — a transient
    /// chain hiccup must not brick the assistant; the reservation tier still
    /// guards concurrent in-flight turns.
    fn committed_day_usd(&self) -> f64 {
        let cutoff = SystemTime::now().checked_sub(DAY);
        let mut total = 0.0;
        let mut cursor = 0u64;
        loop {
            let batch = match self.audit_log.entries_range(cursor, PAGE_SIZE) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!(
                        "aivyx-pa budget: chain read failed at seq={cursor} ({e}); \
                         treating committed day spend as $0 for this turn"
                    );
                    return 0.0;
                }
            };
            if batch.is_empty() {
                break;
            }
            for entry in &batch {
                if let Some(cutoff) = cutoff {
                    if entry.appended_at < cutoff {
                        continue;
                    }
                }
                if let AuditEvent::LlmCost { model, usage, .. } = &entry.event {
                    total += self
                        .pricing
                        .cost_of(model, &to_counts(usage))
                        .usd;
                }
            }
            cursor = batch.last().map(|e| e.seq + 1).unwrap_or(cursor);
        }
        total
    }

    /// Price a conservative estimate for one upcoming turn: the whole output
    /// budget at the model's rate. Local models price at $0, so they never
    /// gate.
    fn estimate_usd(&self, model: &str) -> f64 {
        let counts = TokenCounts {
            output: self.max_tokens as u64,
            ..TokenCounts::default()
        };
        self.pricing.cost_of(model, &counts).usd
    }
}

impl BudgetGate for ChannelBudgetGate {
    fn open_turn(
        &self,
        model: &str,
    ) -> Result<Box<dyn TurnBudgetGuard>, String> {
        let day_usd = self.committed_day_usd();
        let estimate = self.estimate_usd(model);
        match self.enforcer.reserve(day_usd, 0.0, estimate) {
            Ok(reservation) => Ok(Box::new(ChannelTurnGuard {
                enforcer: Arc::clone(&self.enforcer),
                reservation: Some(reservation),
            })),
            Err(verdict) => Err(verdict
                .reason()
                .unwrap_or("budget exceeded")
                .to_string()),
        }
    }
}

/// RAII guard: releases the reservation when the turn ends.
struct ChannelTurnGuard {
    enforcer: Arc<BudgetEnforcer>,
    reservation: Option<Reservation>,
}

impl TurnBudgetGuard for ChannelTurnGuard {}

impl Drop for ChannelTurnGuard {
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            self.enforcer.release(reservation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_audit::{AuditEvent, AuditWriter, PersistentAuditLog};
    use aivyx_cost::BudgetAction;
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{RedbStorage, StorageConfig};

    // A budget config with only a day cap (Deny on exceed).
    fn day_cap(usd: f64) -> BudgetConfig {
        BudgetConfig {
            per_run_usd: None,
            per_day_usd: Some(usd),
            on_exceeded: BudgetAction::Deny,
            alert_at: Some(0.8),
            ..Default::default()
        }
    }

    // A throwaway redb-backed chain (no in-memory Storage exists), seeded with
    // the given LlmCost turns. The temp dir leaks for the test's lifetime —
    // fine for a unit test.
    async fn chain_with_costs(
        costs: &[(&str, TokenUsage)],
    ) -> Arc<PersistentAuditLog> {
        // A UUID, not pid+nanos: parallel tests in the same process can
        // collide on the same nanosecond (coarse clock) → same redb path →
        // a transient flake.
        let dir = std::env::temp_dir()
            .join(format!("aivyx-budget-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let storage = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([7u8; 32]),
        )
        .await
        .expect("open storage");
        let log = PersistentAuditLog::open(storage, [9u8; 32])
            .await
            .expect("open chain");
        for (model, usage) in costs {
            log.append(AuditEvent::LlmCost {
                turn_id: aivyx_core::TurnId::new(),
                model: (*model).to_string(),
                usage: *usage,
            })
            .expect("append");
        }
        Arc::new(log)
    }

    fn usage(input: u32, output: u32) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn no_day_cap_builds_no_gate() {
        let log = chain_with_costs(&[]).await;
        let none = ChannelBudgetGate::new(
            BudgetConfig::default(),
            log,
            Pricing::new(),
            1024,
        );
        assert!(none.is_none(), "no per_day_usd ⇒ no gate");
    }

    #[tokio::test]
    async fn new_gate_boxes_trait_object_when_capped() {
        let log = chain_with_costs(&[]).await;
        // A day cap ⇒ Some boxed gate; the convenience ctor for call sites.
        let some = ChannelBudgetGate::new_gate(
            day_cap(5.0),
            Arc::clone(&log),
            Pricing::new(),
            1024,
        );
        assert!(some.is_some(), "a day cap yields a boxed gate");
        // No cap ⇒ None (callers attach nothing).
        let none = ChannelBudgetGate::new_gate(
            BudgetConfig::default(),
            log,
            Pricing::new(),
            1024,
        );
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn under_cap_allows_turn() {
        // Empty chain ⇒ $0 committed; a $100 day cap with a tiny estimate.
        let log = chain_with_costs(&[]).await;
        let gate =
            ChannelBudgetGate::new(day_cap(100.0), log, Pricing::new(), 1024)
                .expect("gate");
        assert!(gate.open_turn("rated-model").is_ok());
    }

    /// Every model is free without an operator rate (2.0), so the cap
    /// tests price one model by hand.
    fn rated() -> Pricing {
        Pricing::with_overrides([(
            "rated-model".to_string(),
            aivyx_cost::ModelRate::io(15.0, 75.0),
        )])
    }

    #[tokio::test]
    async fn over_cap_denies_turn() {
        // 1M output @ an operator rate of $75/Mtok = $75 already committed;
        // a $50 day cap.
        let log = chain_with_costs(&[("rated-model", usage(0, 1_000_000))]).await;
        let gate =
            ChannelBudgetGate::new(day_cap(50.0), log, rated(), 1024)
                .expect("gate");
        match gate.open_turn("rated-model") {
            Err(err) => {
                assert!(err.contains("day budget"), "denial names the day cap: {err}");
            }
            Ok(_) => panic!("over-cap turn must be denied"),
        }
    }

    #[tokio::test]
    async fn local_model_never_denies() {
        // A huge committed cloud spend but the new turn is on a local model
        // ($0 estimate) — and there's no committed local cost. With a day cap
        // already busted by cloud spend, a local turn is still gated on the
        // SAME committed total, so it would deny. To isolate "$0 estimate",
        // use an empty chain: a local turn reserves $0 and always passes.
        let log = chain_with_costs(&[]).await;
        let gate =
            ChannelBudgetGate::new(day_cap(0.01), log, Pricing::new(), 1_000_000)
                .expect("gate");
        // Even a million-token local turn estimates at $0.
        assert!(gate.open_turn("llama3.1").is_ok());
    }

    #[tokio::test]
    async fn guard_release_frees_reserved_budget() {
        // $10 day cap, empty chain. One rated turn estimating ~$6 reserves;
        // a concurrent second reserve of ~$6 would bust ($12 > $10) — but
        // dropping the first guard frees it.
        let log = chain_with_costs(&[]).await;
        // 80K output @ $75/Mtok = $6.00 estimate.
        let gate =
            ChannelBudgetGate::new(day_cap(10.0), log, rated(), 80_000)
                .expect("gate");
        let g1 = gate.open_turn("rated-model").expect("first reserve");
        // Second concurrent reserve busts the cap.
        assert!(
            gate.open_turn("rated-model").is_err(),
            "two concurrent $6 reserves exceed the $10 cap"
        );
        drop(g1);
        // After release, a fresh reserve fits again.
        assert!(
            gate.open_turn("rated-model").is_ok(),
            "releasing the held reservation frees the budget"
        );
    }
}
