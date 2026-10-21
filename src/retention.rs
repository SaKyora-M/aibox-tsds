//! 生命周期策略：TTL 过期判定与存活下界计算。

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前时间（Unix 毫秒）。
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 记录是否已过期：`timestamp_ms + ttl_ms <= now_ms`。
pub fn is_expired(timestamp_ms: i64, now_ms: i64, ttl_ms: Option<i64>) -> bool {
    match ttl_ms {
        Some(ttl) => timestamp_ms.saturating_add(ttl) <= now_ms,
        None => false,
    }
}

/// TTL 下第一个仍然有效的记录时间戳（用于把查询起点下推）。
pub fn alive_from_ms(now_ms: i64, ttl_ms: Option<i64>) -> Option<i64> {
    ttl_ms.map(|ttl| now_ms.saturating_sub(ttl).saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_boundary_is_inclusive_on_ttl_end() {
        assert!(!is_expired(1_000, 1_999, Some(1_000)));
        assert!(is_expired(1_000, 2_000, Some(1_000)));
        assert!(!is_expired(1_000, i64::MAX, None));
    }

    #[test]
    fn alive_from_skips_exact_expiry_point() {
        assert_eq!(alive_from_ms(2_000, Some(1_000)), Some(1_001));
        assert_eq!(alive_from_ms(500, Some(1_000)), Some(-499));
        assert_eq!(alive_from_ms(2_000, None), None);
    }
}
