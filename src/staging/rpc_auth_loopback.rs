// ============================================================================
// STAGED PATCH — A4-auth-loopback : close auth_middleware loopback bypass
// ============================================================================
//
// WHAT THIS DOES
//   Closes the loopback authentication bypass in `auth_middleware`. Today, when
//   an api_key is configured, any caller whose source IP is loopback (127.0.0.1
//   / ::1) — OR any caller for whom we cannot determine a source IP at all —
//   skips the X-API-Key check entirely. After the F-4 work a node CAN run with
//   a key set (`--rpc-key` is wired), so the bypass means any local process,
//   or anything able to reach 127.0.0.1 (port-forwards, SSH tunnels, sidecar
//   containers sharing the loopback namespace, a compromised localhost service),
//   reaches every RPC route unauthenticated. This patch makes the key MANDATORY
//   for all callers (including loopback) whenever a key is configured.
//
// POLICY (and the trade-off)
//   New behavior is "always enforce when key set". The previous loopback
//   exemption existed for local-CLI convenience (curl from the same box without
//   passing the header). We deliberately drop that convenience: a configured
//   key is an explicit operator decision to require auth, and "trusted because
//   loopback" is exactly the assumption that bites multi-tenant / containerized
//   hosts. Local tooling simply passes `-H "X-API-Key: <key>"` (the operator
//   already knows the key — they set it).
//
//   We expose the opt-out as a single source-level const, NOT a CLI flag,
//   because src/node/src/main.rs is a PROTECTED file and may not gain a new
//   flag in agent work. `ALLOW_LOOPBACK_BYPASS` defaults to `false` (secure).
//   An operator who truly wants the old convenience flips one auditable line
//   and rebuilds. The default ships closed.
//
//   When NO key is configured (api_key == None) the middleware is a pure
//   pass-through, exactly as before — this default path is unchanged.
//
// WHERE THIS WIRES IN
//   File to change : src/node/src/rpc.rs
//   Function       : async fn auth_middleware(...)
//   Doc comment    : rpc.rs ~849-850 (the "Localhost (127.0.0.1) requests
//                    bypass auth" line — replace per PATCH 1 below)
//   Body           : rpc.rs ~851-876 (replace per PATCH 1 below)
//   New const      : add `ALLOW_LOOPBACK_BYPASS` immediately above the function
//   New test       : append the test in PATCH 2 to the existing
//                    `#[cfg(test)] mod tests { ... }` block (after
//                    `rpc_bind_guard_refuses_non_loopback_without_api_key`,
//                    ~line 1628, before the closing brace of the module).
//
// PROTECTED-FILE DEPENDENCY
//   None. rpc.rs is non-protected. No change to main.rs / config.rs /
//   event_loop.rs is required: the api_key plumbing and `--rpc-key` already
//   exist (RpcState.api_key, rpc.rs:96; rpc_bind_guard, rpc.rs:1211).
//
// VERSIONS VERIFIED (src/Cargo.lock)
//   axum 0.8.8  — ConnectInfo<T>(pub T) is a tuple struct; insertable into
//                 request extensions via `req.extensions_mut().insert(...)`.
//   tower 0.5.3 — dev-dep with `util`; `ServiceExt::oneshot` used by tests.
//   tokio 1.50  — `#[tokio::test]`.
//
// LINE ANCHORS VERIFIED (read live on agent-overnight-20260610)
//   rpc.rs:7    `extract::{DefaultBodyLimit, Path, State, ConnectInfo},`
//   rpc.rs:96   `pub api_key: Option<String>,`
//   rpc.rs:849-850  doc comment (see verbatim old below)
//   rpc.rs:851-876  auth_middleware body (see verbatim old below)
//   rpc.rs:1161 `pub fn build_router(rpc_state: Arc<RpcState>) -> Router {`
//   rpc.rs:1168 `.route("/status", get(get_status))`
//   rpc.rs:1200 `.route_layer(... auth_middleware))`
//   rpc.rs:1276 `fn make_rpc_state() -> (Arc<RpcState>, mpsc::Receiver<Transaction>)`
//   rpc.rs:1308 `api_key: None,` (default in the test helper)
//   get_status (rpc.rs:185) returns `Json<ChainStatus>` => 200 OK on the
//   accept path; perfect cheap target for the test assertions.
//
// This file is REFERENCE ONLY. Nothing here is compiled. The founder applies
// PATCH 1 and PATCH 2 to rpc.rs by hand.
// ============================================================================

/* ====================================================================== *
 *  PATCH 1 — auth_middleware: remove loopback bypass when a key is set    *
 *  Target: src/node/src/rpc.rs  ~849-876                                  *
 * ====================================================================== */

/* ---------------------------- OLD (verbatim) -------------------------- *
// ── Feature 15: RPC API key authentication middleware ──

/// Middleware that checks `X-API-Key` header against the configured key.
/// Localhost (127.0.0.1) requests bypass auth. If no key is configured, all requests pass.
async fn auth_middleware(
    State(state): State<Arc<RpcState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    if let Some(ref expected_key) = state.api_key {
        // Bypass auth for localhost.
        let is_localhost = req.extensions()
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().is_loopback())
            .unwrap_or(true); // Default to allowing if we can't determine IP

        if !is_localhost {
            let provided = req.headers()
                .get("X-API-Key")
                .and_then(|v| v.to_str().ok());
            if provided != Some(expected_key.as_str()) {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "invalid or missing API key"})),
                ).into_response();
            }
        }
    }
    next.run(req).await
}
 * ---------------------------------------------------------------------- */

/* ---------------------------- NEW (verbatim) -------------------------- */
// ── Feature 15: RPC API key authentication middleware ──

/// A4-auth-loopback: code-level opt-out for the legacy loopback auth bypass.
///
/// `false` (default, secure) — when an API key is configured the key is
/// required for EVERY caller, including loopback (127.0.0.1 / ::1) and callers
/// whose source IP cannot be determined. This is the correct default: a
/// configured key is an explicit decision to require auth, and "trusted
/// because it came from loopback" is unsafe on multi-tenant / containerized
/// hosts where many unrelated processes share 127.0.0.1.
///
/// `true` — restores the old convenience: loopback callers skip the key check.
/// Only flip this if you fully control every process on the host AND accept
/// that any of them can drive the RPC unauthenticated. There is intentionally
/// no CLI flag for this (src/node/src/main.rs is protected); the opt-out is a
/// single auditable source line that ships closed.
const ALLOW_LOOPBACK_BYPASS: bool = false;

/// Middleware that checks `X-API-Key` header against the configured key.
///
/// If no key is configured, all requests pass (unchanged default).
///
/// A4-auth-loopback: if a key IS configured, the key is required for every
/// caller. Loopback no longer bypasses auth unless `ALLOW_LOOPBACK_BYPASS` is
/// set to `true` at build time. When the bypass is disabled (default) a caller
/// with no determinable source IP is treated as untrusted and must present the
/// key (fail-closed).
async fn auth_middleware(
    State(state): State<Arc<RpcState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    if let Some(ref expected_key) = state.api_key {
        // A4-auth-loopback: by default, enforce for everyone. Only when the
        // build-time opt-out is enabled do we exempt loopback callers.
        let exempt = if ALLOW_LOOPBACK_BYPASS {
            req.extensions()
                .get::<ConnectInfo<std::net::SocketAddr>>()
                .map(|ci| ci.0.ip().is_loopback())
                // Cannot determine the source IP -> fail closed (NOT exempt),
                // even under the opt-out. The old code defaulted this to `true`
                // (exempt), which is exactly the hole this patch closes.
                .unwrap_or(false)
        } else {
            false
        };

        if !exempt {
            let provided = req.headers()
                .get("X-API-Key")
                .and_then(|v| v.to_str().ok());
            if provided != Some(expected_key.as_str()) {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "invalid or missing API key"})),
                ).into_response();
            }
        }
    }
    next.run(req).await
}
/* ---------------------------------------------------------------------- */


/* ====================================================================== *
 *  PATCH 2 — regression tests                                            *
 *  Target: src/node/src/rpc.rs, inside `#[cfg(test)] mod tests { ... }`   *
 *  Append after `rpc_bind_guard_refuses_non_loopback_without_api_key`     *
 *  (~rpc.rs:1628), before the module's closing `}`.                       *
 *                                                                         *
 *  These reuse the EXISTING helpers already in `mod tests`:               *
 *    - make_rpc_state() -> (Arc<RpcState>, mpsc::Receiver<Transaction>)    *
 *    - build_router(state) -> Router          (rpc.rs:1161)               *
 *    - tower::ServiceExt::oneshot             (dev-dep, already imported)  *
 *    - axum::body::Body / axum::http::Request (already imported)          *
 *    - ConnectInfo                            (imported at rpc.rs:7,      *
 *                                              in scope via `use super::*`)*
 *                                                                         *
 *  KEY TEST-DESIGN NOTE: `oneshot` does NOT run the TCP accept path, so   *
 *  nothing inserts a ConnectInfo automatically. The existing tests rely   *
 *  on that (no ConnectInfo => old code's unwrap_or(true) => bypass). To    *
 *  exercise the LOOPBACK path specifically we must explicitly insert a     *
 *  `ConnectInfo(127.0.0.1:NNNN)` into the request extensions. This proves  *
 *  the fix on the exact code path the bug lived on, not on a              *
 *  missing-ConnectInfo artifact.                                          *
 * ====================================================================== */

/* ---------------------------- NEW (verbatim) -------------------------- */
    // ── A4-auth-loopback regression tests ──
    //
    // The bug: when an api_key was set, loopback callers (and callers with no
    // determinable IP) skipped the X-API-Key check. These tests pin the new
    // policy: a configured key is required for loopback callers too, while the
    // no-key default remains a pure pass-through.

    use std::net::SocketAddr;

    /// Helper: a request to GET /status carrying an explicit loopback
    /// ConnectInfo in its extensions (what the real TCP accept path injects).
    /// `key`: Some(..) sets the X-API-Key header; None omits it entirely.
    fn loopback_status_request(key: Option<&str>) -> Request<Body> {
        let loopback: SocketAddr = "127.0.0.1:54321".parse().unwrap();
        let mut builder = Request::builder().method("GET").uri("/status");
        if let Some(k) = key {
            builder = builder.header("X-API-Key", k);
        }
        let mut req = builder.body(Body::empty()).unwrap();
        // Inject the source address exactly as into_make_service_with_connect_info
        // would at runtime, so auth_middleware's is_loopback() path is exercised.
        req.extensions_mut().insert(ConnectInfo(loopback));
        req
    }

    /// With a key configured, a LOOPBACK request that omits X-API-Key MUST be
    /// rejected (401). This is the bypass the patch closes.
    #[tokio::test]
    async fn loopback_without_key_is_rejected_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        // Configure an API key. State Arc is unique here (refcount 1), so
        // get_mut succeeds before build_router clones it into the router.
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let resp = app
            .oneshot(loopback_status_request(None))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "loopback caller with no X-API-Key MUST be rejected when a key is configured \
             (this is the A4 loopback bypass)"
        );
    }

    /// With a key configured, a LOOPBACK request that presents the WRONG key
    /// MUST be rejected (401). Guards against any "present-but-unchecked" path.
    #[tokio::test]
    async fn loopback_with_wrong_key_is_rejected_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let resp = app
            .oneshot(loopback_status_request(Some("wrong-key")))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "loopback caller with a wrong key MUST be rejected when a key is configured"
        );
    }

    /// With a key configured, a LOOPBACK request that presents the CORRECT key
    /// MUST be accepted (200) and reach the handler.
    #[tokio::test]
    async fn loopback_with_correct_key_is_accepted_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let resp = app
            .oneshot(loopback_status_request(Some("s3cret-key")))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "loopback caller presenting the correct key MUST be accepted"
        );
    }

    /// Default path (no key configured): the middleware is a pass-through.
    /// A loopback request with NO X-API-Key MUST still be accepted (200).
    /// This pins that the patch does not regress the unauthenticated default.
    #[tokio::test]
    async fn loopback_passes_through_when_no_key_set() {
        // make_rpc_state() defaults api_key = None (rpc.rs:1308).
        let (state, _rx) = make_rpc_state();
        assert!(state.api_key.is_none(), "helper default must be no-key");
        let app = build_router(state);

        let resp = app
            .oneshot(loopback_status_request(None))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "with no key configured, loopback requests pass through unchanged"
        );
    }

    /// Cross-check: a non-loopback caller with no key is ALSO rejected when a
    /// key is configured. This isn't new behavior (it worked before), but it
    /// pins that the refactor didn't accidentally narrow enforcement to the
    /// loopback branch only.
    #[tokio::test]
    async fn remote_without_key_is_rejected_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let remote: SocketAddr = "203.0.113.7:40000".parse().unwrap();
        let mut req = Request::builder()
            .method("GET")
            .uri("/status")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(remote));

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "non-loopback caller with no key MUST be rejected when a key is configured"
        );
    }
/* ---------------------------------------------------------------------- */
