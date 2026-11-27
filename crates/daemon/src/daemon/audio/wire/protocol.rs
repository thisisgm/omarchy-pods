//! The control frame and tagstruct fields used by our `PulseAudio` client.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::io::{self, Read};

pub(crate) const PROTOCOL_VERSION: u16 = 35;
pub(crate) const CONTROL_CHANNEL: u32 = u32::MAX;
pub(crate) const DESCRIPTOR_SIZE: usize = 20;
pub(crate) const MAX_CHANNELS: usize = 32;
pub(crate) const INDEX_NONE: u32 = u32::MAX;
pub(crate) const FLAG_SHM: u32 = 0x8000_0000;
pub(crate) const FLAG_MEMFD: u32 = 0x4000_0000;

pub(crate) mod tag {
    pub(crate) const STRING: u8 = b't';
    pub(crate) const STRING_NULL: u8 = b'N';
    pub(crate) const U32: u8 = b'L';
    pub(crate) const U8: u8 = b'B';
    pub(crate) const U64: u8 = b'R';
    pub(crate) const S64: u8 = b'r';
    pub(crate) const SAMPLE_SPEC: u8 = b'a';
    pub(crate) const ARBITRARY: u8 = b'x';
    pub(crate) const TRUE: u8 = b'1';
    pub(crate) const FALSE: u8 = b'0';
    pub(crate) const TIMEVAL: u8 = b'T';
    pub(crate) const USEC: u8 = b'U';
    pub(crate) const CHANNEL_MAP: u8 = b'm';
    pub(crate) const CVOLUME: u8 = b'v';
    pub(crate) const PROPLIST: u8 = b'P';
    pub(crate) const VOLUME: u8 = b'V';
    pub(crate) const FORMAT_INFO: u8 = b'f';
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Descriptor {
    pub(crate) length: u32,
    pub(crate) channel: u32,
}

impl Descriptor {
    pub(crate) fn encode(self) -> [u8; DESCRIPTOR_SIZE] {
        let [l0, l1, l2, l3] = self.length.to_be_bytes();
        let [c0, c1, c2, c3] = self.channel.to_be_bytes();
        [
            l0, l1, l2, l3, c0, c1, c2, c3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    }
}

// Sample: 00 00 00 0a ff ff ff ff and 12 zero bytes for a ten-byte control payload.
pub(crate) fn read_descriptor(reader: &mut impl Read) -> io::Result<Descriptor> {
    let mut bytes = [0; DESCRIPTOR_SIZE];
    reader.read_exact(&mut bytes)?;
    let [l0, l1, l2, l3, c0, c1, c2, c3, ..] = bytes;
    Ok(Descriptor {
        length: u32::from_be_bytes([l0, l1, l2, l3]),
        channel: u32::from_be_bytes([c0, c1, c2, c3]),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum CommandTag {
    Error = 0,
    Reply = 2,
    Auth = 8,
    SetClientName = 9,
    GetServerInfo = 20,
    #[cfg(test)]
    GetSinkInfo = 21,
    GetSinkInfoList = 22,
    GetSourceInfoList = 24,
    #[cfg(test)]
    GetModuleInfoList = 26,
    GetClientInfoList = 28,
    GetSourceOutputInfoList = 32,
    Subscribe = 35,
    SetSinkVolume = 36,
    SetDefaultSource = 45,
    SubscribeEvent = 66,
    SuspendSink = 70,
    #[cfg(test)]
    GetCardInfo = 88,
    GetCardInfoList = 89,
    #[cfg(test)]
    SetCardProfile = 90,
}

impl CommandTag {
    #[expect(clippy::as_conversions, reason = "native protocol discriminant")]
    pub(crate) const fn number(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum SinkState {
    #[cfg(test)]
    Running = 0,
    Idle = 1,
    Suspended = 2,
}

impl SinkState {
    #[expect(clippy::as_conversions, reason = "native protocol discriminant")]
    pub(crate) const fn number(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum PulseError {
    #[cfg(test)]
    AccessDenied = 1,
    #[cfg(test)]
    Command = 2,
    #[cfg(test)]
    Invalid = 3,
    NoEntity = 5,
}

impl PulseError {
    #[expect(clippy::as_conversions, reason = "native protocol discriminant")]
    pub(crate) const fn number(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SubscriptionMask(u32);

impl SubscriptionMask {
    pub(crate) const SINK: Self = Self(0x001);
    pub(crate) const SOURCE: Self = Self(0x002);
    pub(crate) const SOURCE_OUTPUT: Self = Self(0x008);
    pub(crate) const CLIENT: Self = Self(0x020);
    pub(crate) const SERVER: Self = Self(0x080);
    pub(crate) const CARD: Self = Self(0x200);

    pub(crate) const fn empty() -> Self {
        Self(0)
    }

    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub(crate) const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub(crate) const fn bits(self) -> u32 {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn from_bits_truncate(bits: u32) -> Self {
        const KNOWN: u32 = 0x03ff;
        Self(bits & KNOWN)
    }
}

impl std::ops::BitOr for SubscriptionMask {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

impl std::ops::BitOrAssign for SubscriptionMask {
    fn bitor_assign(&mut self, other: Self) {
        *self = self.union(other);
    }
}

pub(crate) const EVENT_SEQUENCE: u32 = u32::MAX;
pub(crate) const FACILITY_MASK: u32 = 0x0f;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum SubscriptionEventFacility {
    Sink = 0,
    Source = 1,
    SourceOutput = 3,
    Client = 5,
    Server = 7,
    Card = 9,
}

impl SubscriptionEventFacility {
    #[expect(clippy::as_conversions, reason = "native protocol discriminant")]
    pub(crate) const fn number(self) -> u32 {
        self as u32
    }

    // Sample: 3 is a source output event.
    pub(crate) const fn from_number(number: u32) -> Option<Self> {
        match number {
            0 => Some(Self::Sink),
            1 => Some(Self::Source),
            3 => Some(Self::SourceOutput),
            5 => Some(Self::Client),
            7 => Some(Self::Server),
            9 => Some(Self::Card),
            _ => None,
        }
    }

    pub(crate) const fn mask(self) -> SubscriptionMask {
        SubscriptionMask(1 << self.number())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum SubscriptionEventType {
    New = 0,
    Changed = 0x10,
    Removed = 0x20,
}

impl SubscriptionEventType {
    #[cfg(test)]
    #[expect(clippy::as_conversions, reason = "native protocol discriminant")]
    pub(crate) const fn number(self) -> u32 {
        self as u32
    }

    // Sample: 0x10 is a changed object.
    pub(crate) const fn from_number(number: u32) -> Option<Self> {
        match number {
            0 => Some(Self::New),
            0x10 => Some(Self::Changed),
            0x20 => Some(Self::Removed),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Volume(u32);

impl Volume {
    pub(crate) const NORM: Self = Self(0x10000);
    pub(crate) const MUTED: Self = Self(0);

    pub(crate) const fn as_u32(self) -> u32 {
        self.0
    }

    pub(crate) fn from_u32_clamped(raw: u32) -> Self {
        const MAX_VOLUME: u32 = u32::MAX / 2;
        Self(raw.min(MAX_VOLUME))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChannelVolume {
    count: u8,
    volumes: [Volume; MAX_CHANNELS],
}

impl ChannelVolume {
    pub(crate) const fn empty() -> Self {
        Self {
            count: 0,
            volumes: [Volume::MUTED; MAX_CHANNELS],
        }
    }

    pub(crate) fn push(&mut self, volume: Volume) {
        if let Some(channel) = self.volumes.get_mut(usize::from(self.count)) {
            *channel = volume;
            self.count = self.count.saturating_add(1);
        }
    }

    pub(crate) fn channels(&self) -> &[Volume] {
        self.volumes.get(..usize::from(self.count)).unwrap_or(&[])
    }

    #[cfg(test)]
    pub(crate) fn norm(count: u8) -> Self {
        let mut volumes = Self::empty();
        for _ in 0..count {
            volumes.push(Volume::NORM);
        }
        volumes
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Props(BTreeMap<CString, Vec<u8>>);

impl Props {
    pub(crate) const fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub(crate) fn set_bytes(&mut self, key: impl Into<CString>, value: &[u8]) {
        self.0.insert(key.into(), value.to_vec());
    }

    pub(crate) fn set(&mut self, key: Prop, value: &CStr) {
        let name = match key {
            Prop::ApplicationName => c"application.name",
            #[cfg(test)]
            Prop::DeviceDescription => c"device.description",
            #[cfg(test)]
            Prop::MediaName => c"media.name",
        };
        self.set_bytes(name, value.to_bytes_with_nul());
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&CStr, &[u8])> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_c_str(), value.as_slice()))
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Prop {
    ApplicationName,
    #[cfg(test)]
    DeviceDescription,
    #[cfg(test)]
    MediaName,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct AuthParams {
    pub(crate) version: u16,
    pub(crate) supports_shm: bool,
    pub(crate) supports_memfd: bool,
    pub(crate) cookie: Vec<u8>,
}

impl std::fmt::Debug for AuthParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthParams")
            .field("version", &self.version)
            .field("supports_shm", &self.supports_shm)
            .field("supports_memfd", &self.supports_memfd)
            .finish_non_exhaustive()
    }
}

// Sample: 40000023 is version 35 with memfd.
pub(crate) const fn protocol_version(raw: u32) -> u16 {
    let [_, _, high, low] = raw.to_be_bytes();
    u16::from_be_bytes([high, low])
}

pub(crate) fn version_flags(version: u16, shm: bool, memfd: bool) -> u32 {
    u32::from(version) | if shm { FLAG_SHM } else { 0 } | if memfd { FLAG_MEMFD } else { 0 }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetDeviceVolumeParams {
    pub(crate) device_index: Option<u32>,
    pub(crate) device_name: Option<CString>,
    pub(crate) volume: ChannelVolume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SuspendParams {
    pub(crate) device_index: Option<u32>,
    pub(crate) device_name: Option<CString>,
    pub(crate) suspend: bool,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceLookup {
    pub(crate) index: Option<u32>,
    pub(crate) name: Option<CString>,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetCardProfileParams {
    pub(crate) card_index: Option<u32>,
    pub(crate) card_name: Option<CString>,
    pub(crate) profile_name: CString,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SubscriptionEvent {
    pub(crate) event_facility: SubscriptionEventFacility,
    pub(crate) event_type: SubscriptionEventType,
    pub(crate) index: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Auth(AuthParams),
    SetClientName(Props),
    GetServerInfo,
    #[cfg(test)]
    GetSinkInfo(DeviceLookup),
    GetSinkInfoList,
    GetSourceInfoList,
    #[cfg(test)]
    GetModuleInfoList,
    GetClientInfoList,
    GetSourceOutputInfoList,
    Subscribe(SubscriptionMask),
    SetSinkVolume(SetDeviceVolumeParams),
    SetDefaultSource(CString),
    #[cfg(test)]
    SubscribeEvent(SubscriptionEvent),
    SuspendSink(SuspendParams),
    #[cfg(test)]
    GetCardInfo(DeviceLookup),
    GetCardInfoList,
    #[cfg(test)]
    SetCardProfile(SetCardProfileParams),
}

impl Command {
    pub(crate) const fn tag(&self) -> CommandTag {
        match self {
            Self::Auth(_) => CommandTag::Auth,
            Self::SetClientName(_) => CommandTag::SetClientName,
            Self::GetServerInfo => CommandTag::GetServerInfo,
            #[cfg(test)]
            Self::GetSinkInfo(_) => CommandTag::GetSinkInfo,
            Self::GetSinkInfoList => CommandTag::GetSinkInfoList,
            Self::GetSourceInfoList => CommandTag::GetSourceInfoList,
            #[cfg(test)]
            Self::GetModuleInfoList => CommandTag::GetModuleInfoList,
            Self::GetClientInfoList => CommandTag::GetClientInfoList,
            Self::GetSourceOutputInfoList => CommandTag::GetSourceOutputInfoList,
            Self::Subscribe(_) => CommandTag::Subscribe,
            Self::SetSinkVolume(_) => CommandTag::SetSinkVolume,
            Self::SetDefaultSource(_) => CommandTag::SetDefaultSource,
            #[cfg(test)]
            Self::SubscribeEvent(_) => CommandTag::SubscribeEvent,
            Self::SuspendSink(_) => CommandTag::SuspendSink,
            #[cfg(test)]
            Self::GetCardInfo(_) => CommandTag::GetCardInfo,
            Self::GetCardInfoList => CommandTag::GetCardInfoList,
            #[cfg(test)]
            Self::SetCardProfile(_) => CommandTag::SetCardProfile,
        }
    }

    pub(crate) fn encode(&self, seq: u32) -> io::Result<Vec<u8>> {
        let mut payload = Vec::new();
        let mut tags = TagWriter(&mut payload);
        tags.u32(self.tag().number());
        tags.u32(seq);
        match self {
            Self::Auth(auth) => {
                tags.u32(version_flags(
                    auth.version,
                    auth.supports_shm,
                    auth.supports_memfd,
                ));
                tags.arbitrary(&auth.cookie)?;
            }
            Self::SetClientName(props) => tags.props(props)?,
            Self::Subscribe(mask) => tags.u32(mask.bits()),
            Self::SetSinkVolume(params) => {
                tags.device(params.device_index, params.device_name.as_deref());
                tags.channels(&params.volume);
            }
            Self::SetDefaultSource(name) => tags.string(Some(name)),
            Self::SuspendSink(params) => {
                tags.device(params.device_index, params.device_name.as_deref());
                tags.boolean(params.suspend);
            }
            #[cfg(test)]
            Self::GetSinkInfo(params) | Self::GetCardInfo(params) => {
                tags.device(params.index, params.name.as_deref());
            }
            #[cfg(test)]
            Self::SetCardProfile(params) => {
                tags.device(params.card_index, params.card_name.as_deref());
                tags.string(Some(&params.profile_name));
            }
            #[cfg(test)]
            Self::SubscribeEvent(event) => {
                tags.u32(event.event_facility.number() | event.event_type.number());
                tags.index(event.index);
            }
            #[cfg(test)]
            Self::GetModuleInfoList => {}
            Self::GetServerInfo
            | Self::GetSinkInfoList
            | Self::GetSourceInfoList
            | Self::GetClientInfoList
            | Self::GetSourceOutputInfoList
            | Self::GetCardInfoList => {}
        }
        Ok(payload)
    }
}

#[derive(Debug)]
pub(crate) struct TagWriter<'a>(pub(crate) &'a mut Vec<u8>);

impl TagWriter<'_> {
    pub(crate) fn u32(&mut self, value: u32) {
        self.0.push(tag::U32);
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn boolean(&mut self, value: bool) {
        self.0.push(if value { tag::TRUE } else { tag::FALSE });
    }

    pub(crate) fn string(&mut self, value: Option<&CStr>) {
        if let Some(text) = value {
            self.0.push(tag::STRING);
            self.0.extend_from_slice(text.to_bytes_with_nul());
        } else {
            self.0.push(tag::STRING_NULL);
        }
    }

    fn device(&mut self, index: Option<u32>, name: Option<&CStr>) {
        self.index(index);
        self.string(name);
    }

    pub(crate) fn index(&mut self, index: Option<u32>) {
        self.u32(index.unwrap_or(INDEX_NONE));
    }

    pub(crate) fn arbitrary(&mut self, value: &[u8]) -> io::Result<()> {
        let length = field_length(value.len())?;
        self.0.push(tag::ARBITRARY);
        self.0.extend_from_slice(&length.to_be_bytes());
        self.0.extend_from_slice(value);
        Ok(())
    }

    pub(crate) fn props(&mut self, props: &Props) -> io::Result<()> {
        self.0.push(tag::PROPLIST);
        for (key, value) in props.iter() {
            self.string(Some(key));
            self.u32(field_length(value.len())?);
            self.arbitrary(value)?;
        }
        self.string(None);
        Ok(())
    }

    pub(crate) fn channels(&mut self, volume: &ChannelVolume) {
        self.0.extend_from_slice(&[tag::CVOLUME, volume.count]);
        for channel in volume.channels() {
            self.0.extend_from_slice(&channel.as_u32().to_be_bytes());
        }
    }
}

pub(crate) fn field_length(length: usize) -> io::Result<u32> {
    u32::try_from(length).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}
