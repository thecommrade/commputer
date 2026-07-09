// mempool_quota.rs — per-account pending-tx quota (findings [12]/F-3).
//
// WHAT: a single sender can currently stream unlimited nonce-contiguous txs into
// the mempool (bounded only by the global 5000-tx cap), letting one account crowd
// out every other sender. This module holds the pure per-account quota check so
// the PROTECTED `validate_tx_for_mempool` nonce block stays a one-liner and the
// bound is unit-testable.
//
// WIRING (INERT until the PROTECTED event_loop commit): validate_tx_for_mempool
// (event_loop.rs) computes `pending_from_sender` (already present) and, for any
// sender that is NOT the compiled faucet address, calls
// `account_quota_ok(pending_from_sender, MAX_MEMPOOL_TXS_PER_ACCOUNT)?`. The
// faucet is a trusted internal issuer whose nonce is serialized in rpc.rs and is
// exempt (an admission rejection would strand its next-nonce counter). REJECT,
// never evict — eviction would orphan the sender's higher contiguous nonces.
// FILES NEEDING CHANGES: event_loop.rs (PROTECTED) + `pub mod mempool_quota;` in lib.rs.

/// F-3: maximum number of pending mempool txs a single sender may hold at once.
/// Founder-tunable; 64 is a generous ceiling for a legitimate chained-send burst
/// while still bounding a single account's share of the 5000-tx global mempool.
pub const MAX_MEMPOOL_TXS_PER_ACCOUNT: usize = 64;

/// Reject (never evict) when a sender already has `>= cap` pending txs. Returns
/// the same `Result<(), &'static str>` shape `validate_tx_for_mempool` composes
/// with `?`, so the caller aborts admission of the incoming tx on quota overflow.
pub fn account_quota_ok(pending_from_sender: usize, cap: usize) -> Result<(), &'static str> {
    if pending_from_sender >= cap {
        Err("account mempool quota exceeded")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_cap_is_ok() {
        assert!(account_quota_ok(0, MAX_MEMPOOL_TXS_PER_ACCOUNT).is_ok());
        assert!(account_quota_ok(MAX_MEMPOOL_TXS_PER_ACCOUNT - 1, MAX_MEMPOOL_TXS_PER_ACCOUNT).is_ok());
    }

    #[test]
    fn at_cap_is_rejected() {
        // At the cap the sender already holds `cap` pending — the next tx would be
        // the (cap+1)-th, so admission must be refused.
        assert!(account_quota_ok(MAX_MEMPOOL_TXS_PER_ACCOUNT, MAX_MEMPOOL_TXS_PER_ACCOUNT).is_err());
    }

    #[test]
    fn over_cap_is_rejected() {
        assert!(account_quota_ok(MAX_MEMPOOL_TXS_PER_ACCOUNT + 100, MAX_MEMPOOL_TXS_PER_ACCOUNT).is_err());
    }

    #[test]
    fn error_message_is_stable() {
        // The rogue-binary / ops drill greps for this string; keep it verbatim.
        assert_eq!(
            account_quota_ok(5, 5).unwrap_err(),
            "account mempool quota exceeded"
        );
    }
}
