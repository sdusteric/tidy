//! Pure decisions behind the motions (design section 11): when a motion may play. No GPUI
//! state here, so every rule is unit-tested. Tidy keeps only Haul's row fade-out, the notice
//! band fades and the scanning spinner.

use std::time::Duration;

use crate::theme::motion::REMOVE;

/// How long a trashed row fades before it leaves the list. None means it leaves at once
/// (Reduce Motion).
pub fn remove_delay(reduce_motion: bool) -> Option<Duration> {
    (!reduce_motion).then_some(REMOVE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn rows_fade_for_140_ms_unless_motion_is_reduced() {
        assert_eq!(remove_delay(true), None);
        assert_eq!(remove_delay(false), Some(Duration::from_millis(140)));
    }
}
