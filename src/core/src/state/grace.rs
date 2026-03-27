/// Grace balance tracking for contributor offline tolerance.
///
/// Contributors earn grace by being online. The ratio is 2:1 — every second
/// online earns 2 seconds of grace. Drain is 1:1 — every second offline costs
/// 1 second of grace. Maximum grace balance is 10 years in seconds.

/// 10 years in seconds.
pub const MAX_GRACE_BALANCE: u64 = 10 * 365 * 24 * 3600;

/// Seconds of grace earned per second online.
pub const GRACE_REFILL_RATIO: u64 = 2;

/// Update a contributor's grace balance.
///
/// Order of operations:
/// 1. Drain: subtract `offline_secs` (1:1), saturating at 0.
/// 2. Refill: add `online_secs * GRACE_REFILL_RATIO` (2:1).
/// 3. Cap at `MAX_GRACE_BALANCE`.
pub fn update_grace_balance(current_grace: u64, online_secs: u64, offline_secs: u64) -> u64 {
    let after_drain = current_grace.saturating_sub(offline_secs);
    let after_refill = after_drain.saturating_add(online_secs.saturating_mul(GRACE_REFILL_RATIO));
    after_refill.min(MAX_GRACE_BALANCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grace_accrual() {
        // N seconds online = N * 2 seconds grace (2:1 refill ratio)
        let result = update_grace_balance(0, 100, 0);
        assert_eq!(result, 200);
    }

    #[test]
    fn test_grace_drain() {
        // N seconds offline = N seconds less grace (1:1 drain)
        let result = update_grace_balance(500, 0, 200);
        assert_eq!(result, 300);
    }

    #[test]
    fn test_grace_refill_ratio() {
        // 5 days (432000 secs) online = 10 days (864000 secs) grace (2:1)
        let five_days: u64 = 5 * 24 * 3600;
        let ten_days: u64 = 10 * 24 * 3600;
        let result = update_grace_balance(0, five_days, 0);
        assert_eq!(result, ten_days);
    }

    #[test]
    fn test_grace_max_cap() {
        // Cannot exceed MAX_GRACE_BALANCE (10 years in seconds)
        let result = update_grace_balance(MAX_GRACE_BALANCE, 1_000_000, 0);
        assert_eq!(result, MAX_GRACE_BALANCE);

        // Starting from near the cap, refill should not exceed it
        let result = update_grace_balance(MAX_GRACE_BALANCE - 10, 100, 0);
        assert_eq!(result, MAX_GRACE_BALANCE);
    }

    #[test]
    fn test_grace_floor_zero() {
        // Cannot go below 0
        let result = update_grace_balance(100, 0, 500);
        assert_eq!(result, 0);

        // Completely empty
        let result = update_grace_balance(0, 0, 1000);
        assert_eq!(result, 0);
    }
}
