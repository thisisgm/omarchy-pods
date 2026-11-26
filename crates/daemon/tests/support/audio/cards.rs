//! The fake audio server's cards: the profiles each offers and the active one.

use std::ffi::{CStr, CString};

use pulseaudio::protocol::{CardInfo, CardProfileInfo, Props, PulseError};

/// One profile of a card, with the sinks and sources it would open.
#[derive(Clone, Copy)]
pub(crate) struct FakeProfile {
    pub(crate) name: &'static str,
    pub(crate) available: bool,
    pub(crate) sinks: u32,
    pub(crate) sources: u32,
}

struct Card {
    index: u32,
    name: String,
    profiles: Vec<FakeProfile>,
    active: &'static str,
    props: Props,
}

#[derive(Default)]
pub(super) struct Cards {
    known: Vec<Card>,
    next: u32,
    refusals: u32,
}

fn offered(profiles: &[FakeProfile], name: &str) -> Option<&'static str> {
    let found = profiles.iter().find(|profile| profile.name == name);
    found.map(|profile| profile.name)
}

impl Card {
    /// The index to announce when the profile changed.
    fn activate(&mut self, profile: &'static str) -> Option<u32> {
        let changed = self.active != profile;
        self.active = profile;
        changed.then_some(self.index)
    }

    fn info(&self) -> CardInfo {
        let profiles = self.profiles.iter().map(|profile| CardProfileInfo {
            name: CString::new(profile.name).unwrap(),
            description: None,
            priority: 0,
            available: u32::from(profile.available),
            num_sinks: profile.sinks,
            num_sources: profile.sources,
        });
        CardInfo {
            index: self.index,
            name: CString::new(self.name.as_str()).unwrap(),
            props: self.props.clone(),
            owner_module_index: None,
            driver: None,
            ports: Vec::new(),
            profiles: profiles.collect(),
            active_profile: Some(CString::new(self.active).unwrap()),
        }
    }
}

impl Cards {
    /// The new card's index.
    pub(super) fn add(
        &mut self,
        name: &str,
        profiles: &[FakeProfile],
        active: &str,
        props: Props,
    ) -> u32 {
        assert!(self.find(name).is_none(), "a second card named {name}");
        let active = offered(profiles, active)
            .unwrap_or_else(|| panic!("{name} offers no profile {active}"));
        let index = self.next;
        self.next = self.next.saturating_add(1);
        self.known.push(Card {
            index,
            name: name.to_owned(),
            profiles: profiles.to_vec(),
            active,
            props,
        });
        index
    }

    pub(super) fn remove(&mut self, name: &str) -> u32 {
        let position = self
            .known
            .iter()
            .position(|card| card.name == name)
            .unwrap_or_else(|| panic!("no card named {name}"));
        self.known.remove(position).index
    }

    pub(super) fn active(&self, name: &str) -> &'static str {
        self.named(name).active
    }

    /// As a user sets it, outside any client; the index to announce when the profile changed.
    pub(super) fn set(&mut self, name: &str, profile: &str) -> Option<u32> {
        let card = self.named_mut(name);
        let profile = offered(&card.profiles, profile)
            .unwrap_or_else(|| panic!("{name} offers no profile {profile}"));
        card.activate(profile)
    }

    pub(super) fn refuse(&mut self, times: u32) {
        self.refusals = times;
    }

    /// A client's write, checked in `pipewire-pulse`'s order: card, profile, then permission; the index to announce when the profile changed.
    pub(super) fn write(&mut self, name: &CStr, profile: &CStr) -> Result<Option<u32>, PulseError> {
        let (Ok(name), Ok(profile)) = (name.to_str(), profile.to_str()) else {
            return Err(PulseError::NoEntity);
        };
        let refused = self.refusals > 0;
        let card = self.find_mut(name).ok_or(PulseError::NoEntity)?;
        let profile = offered(&card.profiles, profile).ok_or(PulseError::NoEntity)?;
        if refused {
            self.refusals = self.refusals.saturating_sub(1);
            // corner: pipewire-pulse refuses a card it may not write with access denied
            return Err(PulseError::AccessDenied);
        }
        Ok(card.activate(profile))
    }

    pub(super) fn info(&self, name: &CStr) -> Option<CardInfo> {
        let name = name.to_str().ok()?;
        self.find(name).map(Card::info)
    }

    pub(super) fn list(&self) -> Vec<CardInfo> {
        self.known.iter().map(Card::info).collect()
    }

    fn find(&self, name: &str) -> Option<&Card> {
        self.known.iter().find(|card| card.name == name)
    }

    fn named(&self, name: &str) -> &Card {
        self.find(name)
            .unwrap_or_else(|| panic!("no card named {name}"))
    }

    fn find_mut(&mut self, name: &str) -> Option<&mut Card> {
        self.known.iter_mut().find(|card| card.name == name)
    }

    fn named_mut(&mut self, name: &str) -> &mut Card {
        self.find_mut(name)
            .unwrap_or_else(|| panic!("no card named {name}"))
    }
}
