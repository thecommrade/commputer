use crate::ids::{ParticipantId, hash_parts};

/// Deterministic stake-weighted selection of `count` verifiers from `candidates`,
/// excluding `executor`. `stake_of` returns each candidate's stake (>= 1 assumed; 0 -> treated as 1).
pub fn select_committee(
    seed: &[u8; 32],
    candidates: &[ParticipantId],
    executor: &ParticipantId,
    count: usize,
    stake_of: &dyn Fn(&ParticipantId) -> u64,
) -> Vec<ParticipantId> {
    let mut scored: Vec<(u128, ParticipantId)> = candidates
        .iter()
        .filter(|c| *c != executor)
        .map(|c| {
            let h = hash_parts(&[seed, &c.0]);
            let ticket = u128::from_be_bytes(h[..16].try_into().unwrap());
            let s = stake_of(c).max(1) as u128;
            (ticket / s, *c)
        })
        .collect();
    scored.sort_by_key(|(k, id)| (*k, id.0)); // tie-break on id for determinism
    scored.into_iter().take(count).map(|(_, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    fn pid(n: u8) -> ParticipantId { ParticipantId([n; 32]) }

    #[test]
    fn selection_is_deterministic_and_excludes_executor() {
        let cands = vec![pid(1), pid(2), pid(3), pid(4), pid(5)];
        let stake = |_: &ParticipantId| 1u64;
        let a = select_committee(&[42; 32], &cands, &pid(3), 3, &stake);
        let b = select_committee(&[42; 32], &cands, &pid(3), 3, &stake);
        assert_eq!(a, b);
        assert_eq!(a.len(), 3);
        assert!(!a.contains(&pid(3)));
    }

    #[test]
    fn higher_stake_selected_more_often() {
        // One whale (stake 1000) vs many minnows (stake 1); over many seeds the whale should
        // be selected far more than its 1/N share.
        let mut cands = vec![pid(99)]; // whale
        for n in 1..20u8 { cands.push(pid(n)); }
        let stake = |p: &ParticipantId| if *p == pid(99) { 1000 } else { 1 };
        let mut whale_hits = 0;
        for seed in 0u8..100 {
            let c = select_committee(&[seed; 32], &cands, &pid(200), 3, &stake);
            if c.contains(&pid(99)) { whale_hits += 1; }
        }
        assert!(whale_hits > 60, "whale selected {whale_hits}/100, expected heavy bias");
    }
}
