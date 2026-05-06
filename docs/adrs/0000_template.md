# ADR-NNNN: <Short Decision Title>

<!--
What this file is: An Architecture Decision Record (ADR). It captures one
load-bearing technical decision, why it was made, and what it costs.

Where this should live (after founder review): docs/adrs/ at repo root.

How to use this template:
  1. Copy this file to `NNNN_short_title.md` (NNNN = next free number, zero-padded).
  2. Fill in every section. If a section is genuinely N/A, write "N/A — <one-line reason>".
  3. Keep it ~300-500 words. ADRs are not design docs; they record the *decision*.
  4. Be honest about tradeoffs and known limitations. ADRs that lie about
     downsides are useless to the next contributor.
  5. Cite specific commits (`<short-sha>`) and `path/to/file.rs:LINE` for every
     non-obvious claim. The "References" section is mandatory.
-->

## Status

Proposed | Accepted | Superseded by ADR-NNNN | Deprecated

(One line. If superseded, link the replacement.)

## Context

What forces are at play? What problem are we trying to solve? Describe the
state of the world *before* this decision — including the constraints, the
assumptions we made, and any prior approach that this displaces.

Be specific. "We needed better performance" is useless. "Inbound P2P stalled
~45s during proof ticks because verify_full re-runs iterative_hash for every
(validator × channel)" is useful.

## Decision

What did we decide to do? State it in plain language, in the active voice.

Include the concrete shape of the decision — config values, algorithm names,
file paths, function signatures — anything a future contributor needs to
recognize the decision when they encounter it in the code.

## Consequences

### Positive

- What this buys us. Be concrete.

### Negative

- What this costs us. Be honest. Every decision has costs.
- Performance, complexity, operational burden, attack surface, debt.

### Known Limitations

- Things this decision *does not* solve and that future contributors should
  know about. If the decision can be circumvented, say how.

## Alternatives Considered

Each alternative gets one paragraph: what it was, and *why we did not pick it*.
"We didn't think of it" is not a reason; if an alternative wasn't considered,
omit it. List at least one real alternative; "we had no choice" is almost
never true.

## References

- Commits: `<sha>` <subject line>
- Files: `path/to/file.rs:LINE-RANGE`
- External: links to papers, RFCs, vendor docs that informed the decision
- Related ADRs: ADR-NNNN
