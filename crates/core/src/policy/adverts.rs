//! The pods' adverts while no link is up: when the advert monitor is wanted, and what an advert from our own pods sets.

use pods_proto::addr::BdAddr;
use pods_proto::battery::BatteryReport;
use pods_proto::keys::ProximityKeys;
use pods_proto::model::{Product, product};
use pods_proto::proximity::{self, ADVERT_LEN, Lid};

use super::Effect;
use crate::link::Signal;
use crate::state::{AdvertIntake, LidState, State};

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
    /// No adapter offers advert monitors, or each registration was refused.
    Unavailable,
}

impl MonitorFact {
    pub(super) const fn intake(self) -> AdvertIntake {
        match self {
            Self::Registered => AdvertIntake::Listening,
            Self::Unavailable => AdvertIntake::Unavailable,
        }
    }
}

/// The monitor is wanted while keys are held and the link is not up.
#[derive(Debug, Default)]
pub(super) struct Adverts {
    keys: Option<ProximityKeys>,
    /// From `Signal::Up` to `Signal::Down`, so a channel retry with the device still connected reads adverts.
    link_up: bool,
}

impl Adverts {
    pub(super) const fn new(keys: Option<ProximityKeys>) -> Self {
        Self {
            keys,
            link_up: false,
        }
    }

    pub(super) const fn wanted(&self) -> bool {
        self.keys.is_some() && !self.link_up
    }

    /// The keys an advert is read with, held only while the monitor is wanted.
    pub(super) fn keys(&self) -> Option<&ProximityKeys> {
        self.keys.as_ref().filter(|_| !self.link_up)
    }

    /// `Effect::Monitor` exactly when the signal changes whether the monitor is wanted.
    pub(super) fn follow(&mut self, signal: &Signal) -> Option<Effect> {
        let was = self.wanted();
        match signal {
            Signal::Up { .. } => self.link_up = true,
            Signal::Down => self.link_up = false,
            Signal::Lost { .. } | Signal::Uplink(_) | Signal::Awareness(_) => {}
            Signal::Keys(keys) => self.keys = Some(keys.clone()),
        }
        let now = self.wanted();
        (now != was).then_some(Effect::Monitor(now))
    }
}

/// Sets what the C++ `bleDeviceFound` sets, from an advert our identity key resolves; any other advert changes nothing.
pub(super) fn read(state: &mut State, keys: &ProximityKeys, sighting: &Sighting) {
    if !proximity::resolves(sighting.address, &keys.identity) {
        return;
    }
    let Ok(advert) = proximity::parse(&sighting.data) else {
        return;
    };
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
    if let Ok(report) = decrypted {
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
}

#[cfg(test)]
mod tests;
