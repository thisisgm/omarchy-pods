//! The pods' adverts while no link is up: when the advert monitor is wanted, and what an advert from our own pods sets.

use std::time::Duration;

use pods_proto::addr::BdAddr;
use pods_proto::battery::BatteryReport;
use pods_proto::keys::ProximityKeys;
use pods_proto::model::{Product, product};
use pods_proto::proximity::{self, ADVERT_LEN, ConnectionState, Lid};

use super::Effect;
use crate::link::{SavedKeys, Signal};
use crate::state::{AdvertIntake, LidState, State};

const CONNECT_COOLDOWN_MS: u64 = 30_000;
const MANUAL_HOLD_CLEAR_MS: u64 = 10_000;

/// An advert as BlueZ reported it: where it came from, and the first `ADVERT_LEN` bytes after Apple's company identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sighting {
    pub address: BdAddr,
    pub data: [u8; ADVERT_LEN],
}

/// What BlueZ made of the monitor's registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorFact {
    Registered,
    /// Observation stopped while the monitor registers again.
    Interrupted,
    /// No adapter offers advert monitors, or each registration was refused.
    Unavailable,
}

impl MonitorFact {
    pub(super) const fn intake(self) -> AdvertIntake {
        match self {
            Self::Registered => AdvertIntake::Listening,
            Self::Interrupted => AdvertIntake::Paused,
            Self::Unavailable => AdvertIntake::Unavailable,
        }
    }
}

/// The monitor is wanted while keys are held and the link is not up.
#[derive(Debug, Default)]
pub(super) struct Adverts {
    keys: Option<SavedKeys>,
    /// From `Signal::Up` to `Signal::Down`, so a channel retry with the device still connected reads adverts.
    link_up: bool,
    was_worn: Option<bool>,
    last_connect: Option<Duration>,
    manual_hold: bool,
    out_since: Option<Duration>,
}

impl Adverts {
    pub(super) fn new(keys: Option<SavedKeys>) -> Self {
        Self {
            keys,
            ..Self::default()
        }
    }

    pub(super) const fn wanted(&self) -> bool {
        self.keys.is_some() && !self.link_up
    }

    pub(super) fn read(
        &mut self,
        state: &mut State,
        sighting: &Sighting,
        active_device: Option<BdAddr>,
        now: Duration,
    ) -> Option<BdAddr> {
        let saved = self.keys.as_ref().filter(|_| !self.link_up)?;
        // Legacy keys without an address can supply readings only while no device holds the link.
        if active_device.is_some_and(|device| saved.device != Some(device)) {
            return None;
        }
        let (in_case, connection) = read(state, &saved.keys, sighting)?;
        state.advert_device = saved.device;
        if state.connected || state.advert_intake != AdvertIntake::Listening {
            return None;
        }
        let device = saved.device?;
        let worn = state.ear.any_in_ear();
        let edge = self.was_worn.replace(worn) == Some(false) && worn;
        self.clear_hold(worn, in_case, now);
        (edge
            && connection == ConnectionState::Disconnected
            && !self.manual_hold
            && self.last_connect.is_none_or(|last| {
                now.saturating_sub(last) >= Duration::from_millis(CONNECT_COOLDOWN_MS)
            }))
        .then_some(device)
    }

    pub(super) fn connect_attempted(&mut self, now: Duration) {
        self.last_connect = Some(now);
    }

    pub(super) fn manual_disconnect(&mut self) {
        self.manual_hold = true;
        self.interrupt_wear_observation();
    }

    pub(super) fn interrupt_wear_observation(&mut self) {
        self.was_worn = None;
        self.out_since = None;
    }

    fn clear_hold(&mut self, worn: bool, in_case: bool, now: Duration) {
        if !self.manual_hold {
            return;
        }
        if !worn {
            self.out_since.get_or_insert(now);
        }
        if (!worn && in_case)
            || self.out_since.is_some_and(|since| {
                now.saturating_sub(since) >= Duration::from_millis(MANUAL_HOLD_CLEAR_MS)
            })
        {
            self.manual_hold = false;
            self.out_since = None;
        } else if worn {
            self.out_since = None;
        }
    }

    /// `Effect::Monitor` exactly when the signal changes whether the monitor is wanted.
    pub(super) fn follow(&mut self, signal: &Signal) -> Option<Effect> {
        let was = self.wanted();
        match signal {
            Signal::Up { .. } => {
                self.link_up = true;
                self.manual_hold = false;
                self.interrupt_wear_observation();
            }
            Signal::Down => self.link_up = false,
            Signal::Lost { .. } | Signal::Uplink(_) | Signal::Awareness(_) => {}
            Signal::Keys(keys) => {
                if self.keys.as_ref() != Some(keys) {
                    self.interrupt_wear_observation();
                }
                self.keys = Some(keys.clone());
            }
        }
        let now = self.wanted();
        (now != was).then_some(Effect::Monitor(now))
    }
}

/// Updates status from a resolved proximity advert and returns case evidence and the connection state.
fn read(
    state: &mut State,
    keys: &ProximityKeys,
    sighting: &Sighting,
) -> Option<(bool, ConnectionState)> {
    if !proximity::resolves(sighting.address, &keys.identity) {
        return None;
    }
    let advert = proximity::parse(&sighting.data).ok()?;
    if let Product::AirPods(model) = product(advert.product) {
        state.model = model;
    }
    let headset = state.model.capabilities().is_headset;
    let opened = proximity::open(&advert.sealed, &keys.payload);
    let decrypted = BatteryReport::parse_decrypted(
        &opened,
        advert.primary_left,
        advert.this_pod_in_case,
        headset,
    );
    let mut both_charging = false;
    if let Ok(report) = decrypted {
        both_charging = [report.batteries.left, report.batteries.right]
            .into_iter()
            .all(|charge| charge.is_some_and(|charge| charge.charging));
        state.batteries.apply(&report);
        state.primary = report.primary.or(state.primary);
    }
    // A headset has no case, and its nibble reads 0 rather than the unknown 15.
    if !headset && advert.case.is_some() {
        state.batteries.case = advert.case;
    }
    state.ear = advert.ear;
    state.lid_state = match advert.lid {
        Lid::Open => LidState::Open,
        Lid::Closed => LidState::Closed,
        Lid::Unknown => LidState::Unknown,
    };
    let in_case = advert.this_pod_in_case && (advert.lid == Lid::Closed || both_charging);
    Some((in_case, advert.connection))
}

#[cfg(test)]
mod tests;
