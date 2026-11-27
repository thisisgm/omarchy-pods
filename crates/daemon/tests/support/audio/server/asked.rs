//! The public request log of the fake audio server.

use std::time::Instant;

use crate::support::audio::protocol::{
    Command, DeviceLookup, SetCardProfileParams, SetDeviceVolumeParams, SuspendParams,
};

use super::{Asked, loudest, percent_of};

/// `named` resolves a sink index before the fake records its command.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "the fake records unrecognized commands by number"
)]
pub(super) fn asked(
    command: u32,
    parsed: &Command,
    named: impl FnOnce(u32) -> Option<String>,
) -> Asked {
    match parsed {
        Command::Auth(params) => Asked::Auth {
            version: params.version,
        },
        Command::SetClientName(_) => Asked::ClientName,
        Command::GetClientInfoList => Asked::ClientList,
        Command::GetServerInfo => Asked::ServerInfo,
        Command::GetSinkInfo(DeviceLookup {
            index: None,
            name: Some(name),
        }) => Asked::SinkInfo {
            name: name.to_string_lossy().into_owned(),
        },
        Command::GetSinkInfoList => Asked::SinkList,
        Command::SetSinkVolume(SetDeviceVolumeParams {
            device_index,
            device_name,
            volume,
        }) => {
            let name = match (device_index, device_name) {
                (Some(index), None) => named(*index),
                (None, Some(name)) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            };
            name.map_or(Asked::Unknown { command }, |name| Asked::Volume {
                name,
                index: *device_index,
                percent: percent_of(loudest(volume)),
                at: Instant::now(),
            })
        }
        Command::Subscribe(mask) => Asked::Subscribe { mask: *mask },
        Command::GetCardInfo(DeviceLookup {
            index: None,
            name: Some(name),
        }) => Asked::CardInfo {
            name: name.to_string_lossy().into_owned(),
        },
        Command::GetCardInfoList => Asked::CardList,
        Command::SetCardProfile(SetCardProfileParams {
            card_index: None,
            card_name: Some(card),
            profile_name,
        }) => Asked::Profile {
            card: card.to_string_lossy().into_owned(),
            profile: profile_name.to_string_lossy().into_owned(),
        },
        Command::GetSourceInfoList => Asked::SourceList,
        Command::SetDefaultSource(name) => Asked::DefaultSource {
            name: name.to_string_lossy().into_owned(),
        },
        Command::GetSourceOutputInfoList => Asked::RecorderList,
        Command::SuspendSink(SuspendParams {
            device_index: Some(index),
            device_name: None,
            suspend,
        }) => Asked::Suspend {
            name: named(*index),
            suspend: *suspend,
            at: Instant::now(),
        },
        _ => Asked::Unknown { command },
    }
}
