use super::{
    CATCH_UP_MAX_MS, Duration, FRAMES_PER_MS, NS_PER_SECOND, OPENING_EXIT_MS, Path, RATE_HZ,
    REALIGN_MS, Reader, ns_of, scaled,
};

impl Reader {
    pub(super) fn arm_catch_up(
        &mut self,
        run: u64,
        now: Duration,
        target: i64,
        frames: i64,
        previous: Option<(Duration, i64)>,
    ) {
        if self.heard != Some(run) {
            self.catch_up_left = 0;
            self.catch_up_floor = REALIGN_MS;
        } else if let Some((last, previous_target)) = previous {
            let elapsed = scaled(ns_of(now.saturating_sub(last)), RATE_HZ, NS_PER_SECOND);
            let bound = i64::from(REALIGN_MS.saturating_mul(FRAMES_PER_MS));
            let anchor_advance = target
                .saturating_sub(previous_target)
                .saturating_sub(elapsed);
            let budget = i64::from(CATCH_UP_MAX_MS).saturating_mul(i64::from(FRAMES_PER_MS));
            // Lag past the bound with no budget left is a stall whose budget ran out before its floor, which an advance must not renew.
            let behind_before = target
                .saturating_sub(frames)
                .saturating_sub(self.next)
                .saturating_sub(anchor_advance);
            // A separate stall gets a fresh budget, but an anchor advance cannot renew one.
            if elapsed.saturating_sub(frames) > bound {
                self.catch_up_left = budget;
                self.catch_up_floor = REALIGN_MS;
            } else if self.ring.path() == Path::Dictation
                && self.opening.is_none()
                && self.catch_up_left == 0
                && behind_before <= bound
                && anchor_advance > i64::from(FRAMES_PER_MS)
            {
                self.catch_up_left = budget;
                self.catch_up_floor = OPENING_EXIT_MS;
            }
        }
    }
}
