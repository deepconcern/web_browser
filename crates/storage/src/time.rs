//! Conversion between `SystemTime` and the integer milliseconds stored in the
//! database.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch (negative before it), saturating.
pub(crate) fn to_millis(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_millis())
            .map(|ms| -ms)
            .unwrap_or(i64::MIN),
    }
}

/// The inverse of [`to_millis`]. Values the platform cannot represent clamp
/// to the epoch, which only matters for corrupt data.
pub(crate) fn from_millis(ms: i64) -> SystemTime {
    let d = Duration::from_millis(ms.unsigned_abs());
    let t = if ms >= 0 {
        UNIX_EPOCH.checked_add(d)
    } else {
        UNIX_EPOCH.checked_sub(d)
    };
    t.unwrap_or(UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for ms in [0, 1, -1, 1_700_000_000_123, -86_400_000] {
            assert_eq!(to_millis(from_millis(ms)), ms);
        }
    }

    #[test]
    fn truncates_sub_millisecond() {
        let t = UNIX_EPOCH + Duration::from_nanos(1_500_000);
        assert_eq!(to_millis(t), 1);
    }
}
