//! Pure decision rules for requeue-on-loss (mempool restore).
//!
//! WHAT: the nonce predicate that decides whether a transaction surrendered by
//! a LOSING block candidate may be restored to the mempool.
//! WHERE IT IS WIRED: `event_loop.rs` — `validate_tx_content_inner` calls
//! `nonce_ok_for_requeue` on the restore path; fresh ingress keeps the append
//! rule (`append_nonce_ok`).
//! WHY IT LIVES HERE: `event_loop.rs` is a protected file and needs a live
//! EventLoop to unit-test. The rule is pure arithmetic, so it is testable in
//! isolation — and this is the rule that decides whether the requeue mechanism
//! works at all (see `restore_accepts_the_packed_nonce_while_siblings_wait`).

/// Fresh-ingress rule: a NEW tx queues behind everything its sender already has
/// pooled, so its nonce must be exactly `on_chain + pending_from_sender`.
pub fn append_nonce_ok(tx_nonce: u64, on_chain_nonce: u64, pending_from_sender: usize) -> bool {
    tx_nonce == on_chain_nonce + pending_from_sender as u64
}

/// Restore rule: a tx being put BACK only has to prove it has not already been
/// applied on-chain. Ordering is re-established by the producer's 3-bucket
/// nonce filter at the next pack.
///
/// Using the append rule here would discard exactly the tx the requeue exists
/// to save: block production reads the expected nonce fresh from chain state
/// per tx, so it packs only `nonce == on_chain` and returns the sender's higher
/// nonces to the pool. When that candidate loses, the restored tx carries
/// `nonce == on_chain` while its siblings sit pooled — so the append rule
/// computes a strictly larger expectation and rejects it.
pub fn nonce_ok_for_requeue(tx_nonce: u64, on_chain_nonce: u64) -> bool {
    tx_nonce >= on_chain_nonce
}

/// Rank key for choosing WHICH surrendered txs to spend the requeue budget on,
/// when more were surrendered than the budget can examine. Sorted descending.
///
/// `affordable` leads deliberately. A fee is an unbacked CLAIM at selection
/// time — payability is checked afterwards, and the budget is charged per
/// examination — so ranking on fee alone gives a fabricated `fee: u64::MAX`
/// from a zero-balance key free priority over a real transaction. The faucet
/// dispense this mechanism exists to protect pays exactly MINIMUM_FEE, the
/// floor of that ordering. Requiring the fee to be backed makes the claim cost
/// something.
pub fn requeue_rank(affordable: bool, fee: u64) -> (bool, u64) {
    (affordable, fee)
}

/// Is this sender exempt from fee-payability gating?
///
/// The faucet is a trusted internal issuer whose nonce is serialized in the
/// RPC layer and CONSUMED before the tx reaches the mempool. Rejecting a
/// dispense at ingress therefore strands that nonce and bricks dispensing
/// until the node restarts — so every payability gate must consult this, not
/// just the one it was originally written next to. (A fast-path balance check
/// added without it silently re-broke the faucet once already.)
pub fn payability_exempt(is_faucet_sender: bool, is_validator_register: bool) -> bool {
    is_faucet_sender || is_validator_register
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v4 defect, pinned: an unfunded max-fee tx must NOT outrank a funded
    /// minimum-fee one. Ranking on fee alone hands a costless attacker
    /// deterministic priority over the exact tx being protected.
    #[test]
    fn unbacked_max_fee_ranks_below_a_funded_minimum_fee_tx() {
        const MINIMUM_FEE: u64 = 100_000;
        let honest = requeue_rank(true, MINIMUM_FEE);
        let fabricated = requeue_rank(false, u64::MAX);
        assert!(
            honest > fabricated,
            "a funded floor-fee tx must outrank an unfunded max-fee claim"
        );
        // Fee-only ranking is what got this wrong.
        assert!(u64::MAX > MINIMUM_FEE);
    }

    /// Among txs that can actually pay, higher fee still wins — ordinary
    /// economic priority, and outranking honest traffic now costs real balance.
    #[test]
    fn among_affordable_txs_higher_fee_wins() {
        assert!(requeue_rank(true, 500_000) > requeue_rank(true, 100_000));
        assert!(requeue_rank(false, 500_000) > requeue_rank(false, 100_000));
    }

    /// The faucet must never be rejected by a payability gate — its nonce is
    /// already consumed by the time the tx reaches the mempool, so a rejection
    /// bricks dispensing until restart. Pinned because a fast-path balance
    /// check was once added without this exemption and silently broke it.
    #[test]
    fn faucet_is_exempt_from_every_payability_gate() {
        assert!(payability_exempt(true, false), "faucet sender is exempt");
        assert!(payability_exempt(false, true), "validator registration is exempt");
        assert!(!payability_exempt(false, false), "ordinary senders are gated");
    }

    /// THE case the two rules disagree on, and the reason this module exists.
    /// Sender is at on-chain nonce 10 with 10, 11, 12 pooled. Production packs
    /// 10 (the only one matching chain state) and returns 11 and 12 to the
    /// pool. That candidate loses. Restoring 10 must succeed even though the
    /// append rule would now expect 12.
    #[test]
    fn restore_accepts_the_packed_nonce_while_siblings_wait() {
        let (on_chain, packed_nonce, siblings_pooled) = (10u64, 10u64, 2usize);

        assert!(
            !append_nonce_ok(packed_nonce, on_chain, siblings_pooled),
            "append rule rejects the restored tx — applying it to requeue \
             silently destroys the tx the mechanism exists to save"
        );
        assert!(
            nonce_ok_for_requeue(packed_nonce, on_chain),
            "restore rule must accept it"
        );
    }

    /// A tx whose nonce the chain has already consumed is genuinely dead —
    /// restoring it would resurrect an applied tx every round.
    #[test]
    fn restore_rejects_a_nonce_already_applied_on_chain() {
        assert!(!nonce_ok_for_requeue(9, 10));
        assert!(!nonce_ok_for_requeue(0, 1));
    }

    /// Future nonces are legal on restore (they were legal when first pooled);
    /// the producer's 3-bucket filter holds them back until their predecessor
    /// lands, which is where ordering belongs.
    #[test]
    fn restore_accepts_future_nonces_and_leaves_ordering_to_the_producer() {
        assert!(nonce_ok_for_requeue(11, 10));
        assert!(nonce_ok_for_requeue(u64::MAX, 10));
    }

    /// The append rule is unchanged for fresh ingress.
    #[test]
    fn append_rule_still_pins_fresh_ingress() {
        assert!(append_nonce_ok(10, 10, 0));
        assert!(append_nonce_ok(12, 10, 2));
        assert!(!append_nonce_ok(10, 10, 1));
        assert!(!append_nonce_ok(9, 10, 0));
    }
}
