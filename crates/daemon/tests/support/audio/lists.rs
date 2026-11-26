//! The clients and cards listed by the fake audio server, and their subscription events.

use std::ffi::CString;

use pulseaudio::protocol::{
    ClientInfo, Prop, Props, SubscriptionEventFacility, SubscriptionEventType,
};

use super::cards::FakeProfile;
use super::server::State;

#[derive(Default)]
pub(super) struct Clients {
    pub(super) known: Vec<ClientInfo>,
    next: u32,
}

impl State {
    pub(super) fn add_client(&mut self, hint: Option<&str>) -> u32 {
        let index = self.listed_clients.next;
        self.listed_clients.next = index.saturating_add(1);
        let mut props = Props::new();
        props.set(Prop::ApplicationName, c"Recorder program");
        if let Some(value) = hint {
            let value = CString::new(value).unwrap();
            props.set_bytes(c"session.fast-capture-start", value.as_bytes_with_nul());
        }
        self.listed_clients.known.push(ClientInfo {
            index,
            name: c"Recorder program".to_owned(),
            props,
            ..ClientInfo::default()
        });
        self.announce(
            SubscriptionEventFacility::Client,
            SubscriptionEventType::New,
            Some(index),
        );
        index
    }

    pub(super) fn remove_client(&mut self, index: u32) {
        let clients = &mut self.listed_clients.known;
        let before = clients.len();
        clients.retain(|client| client.index != index);
        assert_eq!(
            before.checked_sub(clients.len()),
            Some(1),
            "fake audio: expected to remove one client at index {index}"
        );
        self.announce(
            SubscriptionEventFacility::Client,
            SubscriptionEventType::Removed,
            Some(index),
        );
    }

    pub(super) fn add_card(
        &mut self,
        name: &str,
        profiles: &[FakeProfile],
        active: &str,
        props: Props,
    ) {
        let index = self.cards.add(name, profiles, active, props);
        self.announce(
            SubscriptionEventFacility::Card,
            SubscriptionEventType::New,
            Some(index),
        );
    }

    pub(super) fn remove_card(&mut self, name: &str) {
        let index = self.cards.remove(name);
        self.announce(
            SubscriptionEventFacility::Card,
            SubscriptionEventType::Removed,
            Some(index),
        );
    }

    pub(super) fn set_profile_as_user(&mut self, card: &str, profile: &str) {
        if let Some(index) = self.cards.set(card, profile) {
            self.announce_card(index);
        }
    }
}
