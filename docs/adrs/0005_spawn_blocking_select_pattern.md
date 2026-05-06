# ADR-0005: `spawn_blocking` + mpsc Channels for CPU-Bound `select!` Arms

## Status

Accepted. Established pattern for any CPU-bound work that originates inside
the event-loop `tokio::select!`.

## Context

`src/node/src/event_loop.rs` is structured as one big `tokio::select!` arm
list: swarm events, block-production tick, proof tick, epoch tick, RPC
events, etc. Each arm is a future polled by a single tokio task. While the
*body* of any one arm runs, no other arms of the same select can fire —
this is fundamental to how `select!` is implemented (arms are polled, the
chosen branch's body runs to completion, then the loop iterates).

Two cases empirically wedged the chain:

1. **Proof tick (Blocker B).** `handle_proof_tick` solved its own and
   incoming peer challenges in-line. CPU/GPU/RAM/Bandwidth/Storage provers
   each take 5-10 seconds; running them serially blocked the swarm-driving
   arm for ~45-50 seconds, during which inbound P2P stalled.

2. **Epoch tick.** `finalize_epoch_with_difficulty` re-runs every prover's
   `verify_full` for every (validator × channel) pair. At ~50 validators ×
   5 channels × ~8s/proof, an epoch transition would pin the worker for
   roughly 30 minutes. Stress run #3 observed 0 blocks produced during a
   62s epoch transition window.

The first attempted fix (commit `d764931`,
`perf(node): wrap epoch finalization in tokio::task::block_in_place`) was
**wrong**. `block_in_place` migrates the calling tokio task to a fresh
worker thread, which helps when the runtime has many independent tasks —
but our event-loop *is one task*. Migrating that task does not let other
arms of its own select fire. Stress run #3 with `block_in_place` still
showed ~110s of stalled block production. Project memory file
`feedback_block_in_place_misuse.md` captures this lesson.

## Decision

For any CPU-bound work originating inside the event-loop `select!`, follow
this three-step pattern:

1. **Extract a pure helper.** Refactor the heavy work into a `'static`
   function that takes owned inputs and returns owned outputs. No `&mut self`
   captures; everything that crosses into the blocking task is by value.
   Examples: `solve_challenge_pure` (commit `a7db3d8`),
   `compute_epoch_verdicts` (commit `5010d5f`).

2. **Add an mpsc channel** carrying the result type. The receiver lives on
   `EventLoop` as a field (e.g. `solver_response_rx`,
   `epoch_finalize_rx`). The sender is cloned into the blocking task.
   See commits `a95e2f4` (solver channel) and `bbbed4f` (epoch channel).

3. **Dispatch and receive.** The original arm body becomes cheap:
   snapshot the inputs and `tokio::task::spawn_blocking` the heavy work,
   sending the result via the mpsc when done. Add a *new* `select!` arm
   that pulls from the receiver and applies the result back to `&mut self`
   in a fast path:

   ```rust
   // Pseudo-shape from event_loop.rs after Blocker B fix:
   Some(resp) = self.solver_response_rx.recv() => {
       self.handle_solver_response(resp);
   }
   ```

This decouples *computation* (running on tokio's blocking-thread pool,
parallelizable with rayon if appropriate) from *application* (running on
the select task with `&mut self` access). The select itself stays
responsive to all other arms throughout.

## Consequences

### Positive

- Block production no longer stalls during epoch transitions or proof
  ticks. Stress run #4 on commit `bbbed4f` produced 12 blocks during
  the 62s epoch transition window vs. 0 on stress #3.
- The pattern is mechanical: any contributor encountering a slow arm
  body can apply it without redesigning the event loop.
- Compatible with rayon: the `spawn_blocking` closure can use
  `par_iter` for further intra-task parallelism.

### Negative

- Each application doubles the surface area: one new arm, one new mpsc
  field, one new `_rx`/`_tx` pair to thread through `EventLoop::new`.
- Backpressure is the contributor's responsibility. Unbounded mpsc =
  potential memory growth if dispatches outpace applications.
- Borrow-checker subtleties: the original arm body must extract owned
  snapshots (`pending_challenges_clone`, `responses_clone`,
  `expired_challenges_clone` were added on `ProofManager` for exactly
  this reason — see commit `bbbed4f`).

### Known Limitations

- The pattern applies cleanly to *fire-and-forget* work where the apply
  phase is much cheaper than the compute phase. Work where compute and
  apply are interleaved (e.g., transaction execution against a live
  state machine) does not fit and would need a different approach.
- We do not currently have a guard against the apply phase itself being
  slow — if `handle_epoch_tick_post` ever grows expensive, we will
  recreate the original problem at smaller scale.

## Alternatives Considered

- **`tokio::task::block_in_place` (commit `d764931`).** Tried first.
  Failed empirically because the event-loop is one task; see Context.
  **Do not retry.**
- **Move the entire event-loop to multiple tasks** (one per concern,
  joined by channels). Would work but is a structural rewrite of the
  node. The select-with-spawn_blocking pattern delivers most of the
  benefit at minimal structural cost.
- **`tokio::spawn` (async task)**. Rejected: the work is CPU-bound, not
  IO-bound. Async tasks share worker threads; CPU-pinning one starves
  every other async task on that worker. `spawn_blocking` uses the
  blocking-thread pool which is sized for exactly this use case.

## References

- Commit `a7db3d8` `feat(proof_manager): add solve_challenge_pure static
  helper for spawn_blocking`
- Commit `82551ad` `perf(node): solve own proof challenges in
  spawn_blocking task`
- Commit `a95e2f4` `feat(node): add solver-response mpsc channel to
  EventLoop`
- Commit `2227d1d` `feat(node): wire solver-response channel into
  event-loop select!`
- Commit `b454744` `perf(node): solve incoming peer proof challenges in
  spawn_blocking too`
- Commit `d764931` `perf(node): wrap epoch finalization in
  tokio::task::block_in_place` (the failed first attempt — kept in
  history as a warning)
- Commit `5010d5f` `perf(proof_manager): parallelize
  compute_epoch_verdicts via rayon::par_iter`
- Commit `f9a125f` `perf(node): wire handle_epoch_tick to parallel
  verdict computation`
- Commit `bbbed4f` `perf(node): defer epoch verdict computation off-task
  via mpsc channel`
- Project memory: `feedback_block_in_place_misuse.md`
- Related: ADR-0002 (Snowball voting itself runs on the select task and
  benefits from the responsiveness this ADR preserves)
