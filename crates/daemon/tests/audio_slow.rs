//! The audio server connection where real time makes it slow: a server restarted after an outage, and one that never answers.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pulseaudio::protocol::MAX_VERSION;

    use super::support::audio::{Asked, CALL_TIMEOUT, LADDER_MS, PERCENT, Volumes};
    use super::support::rig::{Rig, assert_gap, not_before};
    use super::support::{HANG_LIMIT, pause};

    /// One attempt after each step of the ladder, and one more after the repeated last step.
    const ATTEMPTS: usize = 7;
    /// Outages before each restart: one ends as the ladder reaches its last step, the others later in it.
    const OUTAGES_MS: [u64; 3] = [4000, 6500, 9000];
    /// The bound a restarted server must be read again within.
    const UP_AGAIN_WITHIN: Duration = Duration::from_secs(10);

    #[test]
    #[ignore = "a restart after outages of seconds at real time; the slow job runs it"]
    fn a_restarted_server_is_up_again_within_10_s() {
        let (rig, _device, audio) = Rig::with_audio(PERCENT);
        for outage in OUTAGES_MS {
            audio.stop();
            rig.wait_audio_server("down");
            // The outage itself, so the daemon climbs its ladder; nothing is awaited here.
            pause(Duration::from_millis(outage));
            let restarted = Instant::now();
            audio.restart(Volumes::Kept);
            rig.wait_audio_server("up");
            assert_gap(
                &format!("up again after a {outage} ms outage"),
                restarted,
                Instant::now(),
                &(Duration::ZERO..=UP_AGAIN_WITHIN),
            );
        }
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    #[ignore = "seven attempts up the retry ladder at real time; the slow job runs it"]
    fn a_server_that_never_returns_is_retried_no_faster_than_the_ladder() {
        let (rig, device, audio) = Rig::with_audio_holding(PERCENT);
        rig.link_up(&device);
        let longest = CALL_TIMEOUT.saturating_add(Duration::from_millis(LADDER_MS[4]));
        let mut arrivals = Vec::new();
        for count in 1..=ATTEMPTS {
            audio.wait_requests(count, longest.saturating_add(HANG_LIMIT));
            arrivals.push(Instant::now());
        }
        let pairs = arrivals.iter().zip(arrivals.iter().skip(1));
        for (step, (earlier, later)) in pairs.enumerate() {
            let wait = LADDER_MS[step.min(LADDER_MS.len().saturating_sub(1))];
            let wanted = CALL_TIMEOUT.saturating_add(Duration::from_millis(wait));
            assert_gap(
                &format!(
                    "attempt {} after attempt {}",
                    step.saturating_add(2),
                    step.saturating_add(1)
                ),
                *earlier,
                *later,
                &not_before(wanted),
            );
        }
        let auth = Asked::Auth {
            version: MAX_VERSION,
        };
        assert_eq!(audio.requests(), vec![auth; ATTEMPTS]);
        rig.finish();
    }
}
