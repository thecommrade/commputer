# Consensus State Machine (Feature 125)

This document describes the state transitions of the Commputer Snowstorm consensus protocol.

## Overview

Each block height goes through a state machine that determines which block becomes canonical.
The protocol combines Snowball sampling with CRS-weighted anchor selection.

## State Diagram

```
                    +------------------+
                    |                  |
                    |   IDLE / WAIT    |
                    |   (no height)    |
                    |                  |
                    +--------+---------+
                             |
                             | add_candidate()
                             v
                    +------------------+
                    |                  |
                    |  CANDIDATE_ADDED |
                    | (1+ candidates)  |
                    |                  |
                    +--------+---------+
                             |
                     +-------+-------+
                     |               |
                     v               v
            +--------+----+  +------+--------+
            |  SINGLE     |  |  MULTI        |
            |  CANDIDATE  |  |  CANDIDATE    |
            |  (fast path)|  |  (voting)     |
            +------+------+  +------+--------+
                   |                |
                   | immediate      | try_finalize_round()
                   | finalize       | (repeated)
                   |                |
                   v                v
            +------+------+  +------+--------+
            |             |  |  VOTING       |
            |             |  |  ROUNDS       |
            |             |  |  k=3,a=2,b=5  |
            |             |  +------+--------+
            |             |         |
            |             |  +------+-----+-------+
            |             |  |            |       |
            |             |  v            v       v
            |             | quorum      no       timeout
            |             | reached    quorum    (30s)
            |             |  |          |         |
            |             |  v          v         |
            |             | count++   count=0     |
            |             |  |          |         |
            |             |  +-----+----+         |
            |             |        |              |
            |             |  count >= beta?       |
            |             |  |           |        |
            |             |  yes         no       |
            |             |  |           |        |
            |             |  v           v        |
            +------+------+  +----+     loop     |
                   |              |      back    |
                   |              |              |
                   v              v              v
            +------+--------------+--------------+
            |                                    |
            |           FINALIZED                |
            |     (winning block chosen)         |
            |                                    |
            +----------------+-------------------+
                             |
                             | take_finalized()
                             v
                    +--------+---------+
                    |                  |
                    |  APPLIED TO      |
                    |  CHAIN STATE     |
                    |                  |
                    +------------------+
```

## View Change Sub-State

```
            +------------------+
            |  WAITING FOR     |
            |  BLOCK PRODUCER  |
            +--------+---------+
                     |
                     | 10s elapsed,
                     | no block received
                     v
            +--------+---------+
            |  VIEW CHANGE     |
            |  TRIGGERED       |
            +--------+---------+
                     |
                     | next CRS validator
                     | takes over
                     v
            +--------+---------+
            |  NEW PRODUCER    |
            |  PRODUCES BLOCK  |
            +------------------+
```

## Equivocation Detection

```
            +------------------+
            |  add_candidate() |
            +--------+---------+
                     |
                     | check (validator, height)
                     v
            +--------+---------+
            | existing hash?   |
            +--+------------+--+
               |            |
               no           yes
               |            |
               v            v
          +----+----+  +----+--------+
          | record  |  | same hash?  |
          | mapping |  +--+-------+--+
          +---------+     |       |
                          yes     no
                          |       |
                          v       v
                     +----+--+ +--+----------+
                     | skip  | | EQUIVOCATION|
                     | (dup) | | -> SLASH    |
                     +-------+ +-------------+
```

## Finality Sub-State (Feature 124)

```
            +------------------+
            |  BLOCK FINALIZED |
            |  (consensus)     |
            +--------+---------+
                     |
                     | finality votes from
                     | validators
                     v
            +--------+---------+
            | 2/3+ weight      |
            | confirmed?       |
            +--+------------+--+
               |            |
               no           yes
               |            |
               v            v
          +----+------+ +---+------------+
          | continue  | | FINAL          |
          | collecting| | (no reorg past |
          | votes     | |  this point)   |
          +-----------+ +----------------+
```

## Parameters

| Parameter | Symbol | Testing | Production | Description |
|-----------|--------|---------|------------|-------------|
| Sample size | k | 3 | 20 | Peers polled per round |
| Quorum | alpha | 2 | 14 | Agreement threshold |
| Decision threshold | beta | 5 | 20 | Consecutive rounds |
| Consensus timeout | - | 10s | 30s | Force finalization |
| View change timeout | - | 5s | 10s | Producer offline |
| Finality depth | - | 10 | 100 | Reorg protection |

## Transitions Summary

1. **IDLE -> CANDIDATE_ADDED**: When `add_candidate()` is called with a new block.
2. **CANDIDATE_ADDED -> FINALIZED**: Single candidate fast-path (immediate).
3. **CANDIDATE_ADDED -> VOTING**: Multiple candidates trigger Snowball voting.
4. **VOTING -> FINALIZED**: After beta consecutive rounds with quorum agreement.
5. **VOTING -> VOTING**: No quorum in a round resets the consecutive counter.
6. **VOTING -> FINALIZED**: Timeout (30s) forces finalization on current preference.
7. **FINALIZED -> APPLIED**: `take_finalized()` removes the block and applies it.
