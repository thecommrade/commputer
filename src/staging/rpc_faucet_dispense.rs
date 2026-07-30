// ============================================================================
// ⚠⚠ SUPERSEDED AND WRONG — DO NOT COPY FROM THIS FILE. ⚠⚠
//
// The real faucet shipped long ago and lives at src/node/src/rpc.rs
// (`faucet()` + `build_faucet_transfer`). Read THAT.
//
// This blueprint teaches a rule that is FALSE and that cost this project
// months of a silently dead faucet. Below it asserts:
//     "Transfer is NOT fee-exempt: fee must be >= MINIMUM_FEE (=100_000)"
// and builds the dispense with `fee: MINIMUM_FEE`. Consensus rejects a
// Transfer to an account that does not yet exist unless the fee is at least
// ACCOUNT_CREATION_FEE (1_000_000) — and a faucet dispense is ALWAYS to a new
// account. The mempool only checks MINIMUM_FEE, so such a tx is accepted by
// every node, gossiped, selected into a block, and then dropped at apply.
// Until 2026-07-28 that drop was completely unlogged, so EVERY dispense in the
// project's history failed invisibly. The test at the bottom of this file
// asserts `tx.fee >= MINIMUM_FEE`, which the broken value satisfies — that is
// precisely why the suite stayed green.
//
// Kept only as a record of the mistake. Not in the workspace members list, so
// it never compiles.
// ============================================================================
//
// A3-real-faucet — staged faucet dispense handler (REVIEW ONLY, do not compile
// as-is). This file is reference material for the founder; it targets the
// POST-EDIT shape of RpcState described in src/staging/docs/real_faucet_blueprint.md.
//
// WHAT IT DOES:
//   Replaces the honest-503 body of `faucet()` in src/node/src/rpc.rs (~:721)
//   with a real dispenser that builds + signs a TxKind::Transfer of 1 COMME and
//   queues it on `state.tx_sender`. Keeps the honest 503 when no faucet wallet
//   is provisioned. Records the per-epoch claim ONLY on a successful queue.
//
// WHERE IT WIRES IN:
//   - Paste `faucet()` over the existing one at src/node/src/rpc.rs:721-767.
//   - Paste `build_faucet_transfer()` as a sibling private fn in rpc.rs.
//   - Paste the `#[cfg(test)]` test into rpc.rs's existing `mod tests`
//     (next to faucet_does_not_lie_when_unprovisioned at rpc.rs:1631).
//
// EXISTING FILE THAT MUST CHANGE FIRST (non-protected):
//   src/node/src/rpc.rs — add to `struct RpcState` (after :94):
//       pub faucet_wallet: Option<commputer_core::wallet::Wallet>,
//       pub faucet_next_nonce: tokio::sync::Mutex<u64>,
//     and update BOTH RpcState literals (main.rs:1107 + make_rpc_state at
//     rpc.rs:1276) to initialize them.
//
// PROTECTED-FILE DEPENDENCY (founder-only; see blueprint sections 4 & 5):
//   - src/node/src/main.rs:1107 — derive faucet_wallet from COMMPUTER_FAUCET_SEED,
//     seed faucet_next_nonce from chain state.
//   - genesis funding path (genesis.json + src/core/src/genesis.rs +
//     src/storage/src/state.rs) so the faucet address actually holds COMME.
//     Without a funded faucet account, queued transfers fail balance checks in
//     apply_transaction and never confirm (the 200 is then a queued-but-doomed tx).
//
// VERIFIED FACTS (working tree, branch agent-overnight-20260610):
//   - tx.verify() (core/transaction.rs:213) signs (from||nonce||kind||fee), NO
//     chain_id — matches sign_transaction (core/signing.rs:30). So a faucet tx
//     signed with sign_transaction passes the mempool's tx.verify() gate
//     (event_loop.rs:2113).
//   - Transfer is NOT fee-exempt: fee must be >= MINIMUM_FEE (=100_000)
//     (transaction.rs:148; event_loop.rs:2131-2136).
//   - Strict nonce: tx.nonce must == on_chain_nonce + pending_from_sender
//     (event_loop.rs:2137-2149) — hence the runtime faucet_next_nonce counter.
//   - UNITS_PER_COMME = 100_000_000 (token.rs:8); Amount::from_raw is const
//     (token.rs:100). 1 COMME = Amount::from_raw(UNITS_PER_COMME).
// ============================================================================

use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;
use tokio::sync::mpsc;

use commputer_core::identity::Address;
use commputer_core::signing::sign_transaction;
use commputer_core::token::{Amount, UNITS_PER_COMME};
use commputer_core::transaction::{Transaction, TxKind, MINIMUM_FEE};
use commputer_core::wallet::Wallet;

use crate::rpc::{FaucetRequest, RpcState};

/// Build and sign a faucet Transfer of exactly 1 COMME to `to`, using `nonce`
/// as the transaction nonce. Fee is set to MINIMUM_FEE because Transfers are not
/// fee-exempt in the mempool. Signed with `sign_transaction`, which produces the
/// no-chain_id signature that tx.verify() (the mempool gate) checks.
fn build_faucet_transfer(wallet: &Wallet, to: Address, nonce: u64) -> Transaction {
    let mut tx = Transaction {
        from: *wallet.address(),
        nonce,
        kind: TxKind::Transfer {
            to,
            // 1 COMME in raw units. Amount::from_raw(UNITS_PER_COMME) == 1 COMME.
            amount: Amount::from_raw(UNITS_PER_COMME),
        },
        fee: MINIMUM_FEE,
        signature: vec![],
        public_key: vec![],
        memo: None,
        timelock: None,
    };
    sign_transaction(&mut tx, wallet);
    tx
}

/// POST /faucet — dispense 1 COMME of testnet COMME from the provisioned faucet
/// wallet. Returns an honest 503 when no faucet wallet is configured.
async fn faucet(
    State(state): State<Arc<RpcState>>,
    Json(req): Json<FaucetRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if !state.is_testnet {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": "faucet only available on testnet",
        })));
    }

    // Validate address format (64 hex chars / 32 bytes) and parse it.
    let to = match Address::from_hex(&req.address) {
        Ok(addr) => addr,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "invalid address format (expected 64 hex characters)",
            })));
        }
    };

    let current_epoch = state.status.lock().await.epoch;

    // Rate limit: 1 request per address per epoch. Read-only check here; the
    // claim slot is only CONSUMED after a successful queue (below), so a request
    // we cannot fulfill never burns the caller's epoch slot.
    {
        let claims = state.faucet_claims.lock().await;
        if let Some(&last_epoch) = claims.get(&req.address)
            && last_epoch >= current_epoch {
                return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({
                    "error": "faucet already claimed this epoch",
                    "next_available_epoch": current_epoch + 1,
                })));
            }
    }

    // HONESTY PATH (W5.7 F-6): no provisioned signing wallet -> 503, no claim.
    let Some(faucet_wallet) = state.faucet_wallet.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
            "error": "faucet not provisioned",
            "detail": "no faucet wallet is configured on this node; tokens cannot be dispensed",
            "address": req.address,
            "epoch": current_epoch,
        })));
    };

    // DISPENSE PATH. Hold the nonce lock across build + try_send so concurrent
    // claims get distinct, contiguous nonces (see blueprint section 3). The lock
    // is released at end of scope before we touch faucet_claims.
    let mut next_nonce = state.faucet_next_nonce.lock().await;
    let nonce = *next_nonce;
    let tx = build_faucet_transfer(faucet_wallet, to, nonce);
    let tx_hash = hex::encode(tx.hash().0);

    match state.tx_sender.try_send(tx) {
        Ok(()) => {
            // Only now is the nonce truly consumed.
            *next_nonce = next_nonce.saturating_add(1);
            drop(next_nonce);

            // Record the per-epoch claim ONLY on success.
            state.faucet_claims.lock().await.insert(req.address.clone(), current_epoch);

            (StatusCode::OK, Json(serde_json::json!({
                "success": true,
                "address": req.address,
                "amount": UNITS_PER_COMME,            // 1 COMME in raw units
                "amount_comme": 1,
                "tx_hash": tx_hash,
                "epoch": current_epoch,
            })))
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            // Do NOT consume nonce or claim — caller can retry.
            (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
                "error": "transaction queue full, try again later",
                "address": req.address,
                "epoch": current_epoch,
            })))
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": "node is shutting down",
            })))
        }
    }
}

// ============================================================================
// TEST (paste into rpc.rs `#[cfg(test)] mod tests`). Requires make_rpc_state to
// be extended to accept/expose a faucet wallet. The minimal change: give
// make_rpc_state an overload that sets `faucet_wallet: Some(w)` and
// `faucet_next_nonce: Mutex::new(0)`. Shown inline below as make_rpc_state_with_faucet.
//
// This test exercises REAL behavior: a provisioned faucet must (a) return 200
// with success:true and (b) actually queue a signed TxKind::Transfer of 1 COMME
// to the requested address on tx_sender, with a valid signature (tx.verify()),
// correct fee (>= MINIMUM_FEE), and nonce 0. The unprovisioned case is already
// covered by faucet_does_not_lie_when_unprovisioned (rpc.rs:1631).
// ============================================================================
#[cfg(test)]
mod faucet_dispense_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    use std::collections::HashMap;
    use tokio::sync::{broadcast, mpsc, Mutex};
    use std::time::Instant;
    use commputer_core::wallet::Wallet;
    use commputer_core::transaction::{Transaction, TxKind, MINIMUM_FEE};
    use commputer_core::token::UNITS_PER_COMME;
    use commputer_core::identity::Address;

    // Mirror of make_rpc_state (rpc.rs:1276) but with a provisioned faucet wallet.
    // When pasting into rpc.rs, prefer factoring make_rpc_state to take an
    // Option<Wallet> instead of duplicating; duplicated here only so this staged
    // file documents the full required state shape.
    fn make_rpc_state_with_faucet(
        faucet_wallet: Option<Wallet>,
    ) -> (Arc<RpcState>, mpsc::Receiver<Transaction>) {
        let (tx_sender, rx) = mpsc::channel(16);
        let state = Arc::new(RpcState {
            tx_sender,
            status: Mutex::new(super::super::ChainStatus {
                height: 42, total_supply: 2_000_000_000, emitted: 1000, burned: 50,
                circulating: 950, remaining: 1_999_999_000, accounts: 3, epoch: 1,
                pending_txs: 0,
            }),
            peers: Mutex::new(vec![]),
            balances: Mutex::new(HashMap::new()),
            mempool: Mutex::new(vec![]),
            blocks: Mutex::new(HashMap::new()),
            receipts: Mutex::new(HashMap::new()),
            metrics: Mutex::new(super::super::NodeMetrics {
                uptime_secs: 0, height: 0, epoch: 0, peers_connected: 0,
                peers_banned: 0, blocks_produced: 0, pending_txs: 0, seen_tx_count: 0,
            }),
            compliance_stats: Mutex::new(super::super::ComplianceDashboard::default()),
            anti_scale_metrics: Mutex::new(super::super::AntiScaleDashboard::default()),
            network_health: Mutex::new(super::super::NetworkHealthDashboard::default()),
            peer_quality: Mutex::new(HashMap::new()),
            storage_metrics: Mutex::new(commputer_storage::StorageMetrics::default()),
            ws_broadcast: broadcast::channel(256).0,
            is_testnet: true,
            faucet_claims: Mutex::new(HashMap::new()),
            api_key: None,
            rate_limits: Mutex::new(HashMap::new()),
            validator_performance: Mutex::new(HashMap::new()),
            cors_origins: "*".to_string(),
            start_time: Instant::now(),
            chain_health: Mutex::new(serde_json::json!({})),
            traffic_stats: Mutex::new(serde_json::json!({})),
            proof_history: Mutex::new(HashMap::new()),
            proof_leaderboard: Mutex::new(HashMap::new()),
            capacity: Mutex::new((0, 0, 0, 0)),
            // A3 new fields:
            faucet_wallet,
            faucet_next_nonce: Mutex::new(0),
        });
        (state, rx)
    }

    #[tokio::test]
    async fn faucet_dispenses_a_signed_transfer_when_provisioned() {
        let faucet = Wallet::generate();
        let faucet_addr = *faucet.address();
        let (state, mut rx) = make_rpc_state_with_faucet(Some(faucet));
        let app = super::super::build_router(state);

        let recipient = [9u8; 32];
        let recipient_hex = hex::encode(recipient);
        let body = serde_json::to_vec(&serde_json::json!({ "address": recipient_hex })).unwrap();

        let req = Request::builder()
            .method("POST").uri("/faucet")
            .header("content-type", "application/json")
            .body(Body::from(body)).unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["amount"].as_u64().unwrap(), UNITS_PER_COMME);

        // A real, signed Transfer of 1 COMME must have been queued on tx_sender.
        let tx = rx.try_recv().expect("faucet must queue a tx when provisioned");
        assert!(tx.verify(), "queued faucet tx must have a valid signature");
        assert_eq!(tx.from, faucet_addr, "tx must be from the faucet wallet");
        assert_eq!(tx.nonce, 0, "first dispense uses nonce 0");
        assert!(tx.fee >= MINIMUM_FEE, "Transfer is not fee-exempt; fee must cover MINIMUM_FEE");
        match tx.kind {
            TxKind::Transfer { to, amount } => {
                assert_eq!(to, Address(recipient));
                assert_eq!(amount.raw(), UNITS_PER_COMME, "must dispense exactly 1 COMME");
            }
            other => panic!("expected Transfer, got {:?}", other),
        }
        // tx_hash echoed to the client matches the queued tx.
        assert_eq!(v["tx_hash"], hex::encode(tx.hash().0));
    }

    #[tokio::test]
    async fn faucet_without_wallet_still_returns_503() {
        // Regression mirror of rpc.rs:1631 against this handler: no wallet -> 503,
        // nothing queued.
        let (state, mut rx) = make_rpc_state_with_faucet(None);
        let app = super::super::build_router(state);

        let body = serde_json::to_vec(&serde_json::json!({
            "address": hex::encode([7u8; 32])
        })).unwrap();
        let req = Request::builder()
            .method("POST").uri("/faucet")
            .header("content-type", "application/json")
            .body(Body::from(body)).unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.get("success").is_none());
        assert_eq!(v["error"], "faucet not provisioned");
        assert!(rx.try_recv().is_err(), "must not queue a tx when unprovisioned");
    }
}
