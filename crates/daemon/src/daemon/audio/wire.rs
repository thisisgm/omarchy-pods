//! Control frames and reply readers for the `PulseAudio` native protocol.

#![forbid(unsafe_code)]

use std::ffi::CStr;
use std::fmt;
use std::io::{self, ErrorKind};
use std::iter;

use pods_core::policy::audio::{Percent, SinkState};
use pods_proto::addr::BdAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// A card list is a few kilobytes; a larger frame ends the connection.
pub(super) const AUDIO_FRAME_MAX: usize = 262_144;

/// Server name and version, user name, host name and sample spec, before the default sink.
const BEFORE_DEFAULT_SINK: usize = 5;

/// Description, sample spec, channel map and owner module, between a sink's name and its volume.
const BEFORE_VOLUME: usize = 4;

/// Monitor name, two latencies, driver, flags, properties and base volume.
const BEFORE_STATE: usize = 7;

/// Volume steps and card.
const AFTER_STATE: usize = 2;

/// Name, description, priority, availability, availability group and type.
const PORT_FIELDS: usize = 6;

/// A desktop holds a few dozen clients; refuse a longer list whole.
pub(super) const MAX_CLIENTS: usize = 256;
const FAST_CAPTURE: &[u8] = b"session.fast-capture-start";
const HEADSET_PREFIX: &[u8] = b"headset-";
/// Index, name, owner module and driver before a client's properties.
const BEFORE_CLIENT_PROPERTIES: usize = 4;
/// The owner module and driver before the list of card profiles.
const BEFORE_CARD_PROFILES: usize = 2;
/// Name, description, sink and source counts, priority and availability.
const PROFILE_FIELDS: usize = 6;
/// Name, description, priority, availability, direction and properties.
const BEFORE_PORT_PROFILES: usize = 6;
/// Latency offset, availability group and type.
const AFTER_PORT_PROFILES: usize = 3;

/// The percent that `Volume::NORM` stands for.
const FULL: u64 = 100;

pub(super) mod protocol;

#[cfg(test)]
use protocol as native;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "unit fixtures share their codec with the external fake server and client"
)]
#[path = "../../../tests/support/audio/protocol.rs"]
pub(super) mod fixture;

pub(super) use protocol::PROTOCOL_VERSION;
use protocol::tags::TagError;
pub(super) use protocol::tags::Tags;
use protocol::{
    CONTROL_CHANNEL, ChannelVolume, Command, CommandTag, DESCRIPTOR_SIZE, Descriptor,
    EVENT_SEQUENCE, FACILITY_MASK, SubscriptionEventFacility, SubscriptionEventType,
    SubscriptionMask, Volume, field_length, protocol_version, read_descriptor,
};

/// What is wrong with the server's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Malformed {
    Oversized(u32),
    StreamData(u32),
    Field(TagError),
    Nameless,
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversized(length) => {
                write!(f, "a frame of {length} bytes, past {AUDIO_FRAME_MAX}")
            }
            Self::StreamData(channel) => write!(f, "stream data on channel {channel}"),
            Self::Field(error) => error.fmt(f),
            Self::Nameless => f.write_str("a sink with no name"),
        }
    }
}

impl From<TagError> for Malformed {
    fn from(error: TagError) -> Self {
        Self::Field(error)
    }
}

/// One message's payload; its descriptor is stripped on read and made on write.
#[derive(Debug)]
pub(super) struct Frame(Vec<u8>);

impl Frame {
    pub(super) fn request(seq: u32, command: &Command) -> io::Result<Self> {
        command.encode(seq).map(Self)
    }
}

/// Cancel safe: what a cancelled read took stays in `pending` for the next.
pub(super) async fn read_frame(
    stream: &mut UnixStream,
    pending: &mut Vec<u8>,
) -> io::Result<Frame> {
    loop {
        if let Some(frame) = take_frame(pending)? {
            return Ok(frame);
        }
        if stream.read_buf(pending).await? == 0 {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "the server closed the connection",
            ));
        }
    }
}

/// The first whole frame in `pending`, its length held to `AUDIO_FRAME_MAX` before its payload is awaited; sample: a descriptor declaring 10 bytes, then 10 bytes.
pub(super) fn take_frame(pending: &mut Vec<u8>) -> io::Result<Option<Frame>> {
    let Some(descriptor) = pending.first_chunk::<DESCRIPTOR_SIZE>() else {
        return Ok(None);
    };
    let length = payload_length(descriptor)
        .map_err(|malformed| io::Error::new(ErrorKind::InvalidData, malformed.to_string()))?;
    let end = DESCRIPTOR_SIZE.saturating_add(length);
    if pending.len() < end {
        return Ok(None);
    }
    Ok(Some(Frame(
        pending.drain(..end).skip(DESCRIPTOR_SIZE).collect(),
    )))
}

pub(super) async fn write_frame(stream: &mut UnixStream, frame: &Frame) -> io::Result<()> {
    let descriptor = Descriptor {
        length: field_length(frame.0.len())?,
        channel: CONTROL_CHANNEL,
    };
    stream.write_all(&descriptor.encode()).await?;
    stream.write_all(&frame.0).await
}

/// The payload length a descriptor declares; sample `00 00 00 0a ff ff ff ff` then 12 zero bytes: 10 bytes of commands.
fn payload_length(descriptor: &[u8; DESCRIPTOR_SIZE]) -> Result<usize, Malformed> {
    let Descriptor {
        length, channel, ..
    } = read_descriptor(&mut descriptor.as_slice())
        .ok()
        .ok_or(TagError::Cut)?;
    if channel != CONTROL_CHANNEL {
        return Err(Malformed::StreamData(channel));
    }
    usize::try_from(length)
        .ok()
        .filter(|length| *length <= AUDIO_FRAME_MAX)
        .ok_or(Malformed::Oversized(length))
}

/// A frame as this client tells it apart.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Message<'a> {
    Reply {
        seq: u32,
        body: &'a [u8],
    },
    Refused {
        seq: u32,
        code: u32,
    },
    /// The facility that changed, or none when the event could invalidate anything.
    Event(Option<SubscriptionMask>),
    /// A command this client never asks about, as a stream's.
    Other,
}

/// A frame's command, its sequence and what follows; sample `L 00000002 L 00000005` then the values: the answer to request 5.
pub(super) fn message(frame: &Frame) -> Result<Message<'_>, Malformed> {
    let mut tags = Tags::new(&frame.0);
    let number = tags.u32()?;
    if number == CommandTag::SubscribeEvent.number() {
        return Ok(Message::Event(event(tags)));
    }
    let seq = tags.u32()?;
    Ok(match number {
        number if number == CommandTag::Reply.number() => Message::Reply {
            seq,
            body: tags.remaining(),
        },
        number if number == CommandTag::Error.number() => Message::Refused {
            seq,
            code: tags.u32()?,
        },
        _ => Message::Other,
    })
}

/// An event's sequence, facility and type flags, and index; sample `L ffffffff L 00000010 L 00000007`: sink 7 changed.
fn event(mut tags: Tags<'_>) -> Option<SubscriptionMask> {
    if tags.u32().ok()? != EVENT_SEQUENCE {
        return None;
    }
    let flags = tags.u32().ok()?;
    tags.u32().ok()?;
    if !tags.is_empty() {
        return None;
    }
    SubscriptionEventType::from_number(flags & !FACILITY_MASK)?;
    Some(SubscriptionEventFacility::from_number(flags & FACILITY_MASK)?.mask())
}

/// The version an auth reply grants, its high half being memory flags; sample `L 40 00 00 23`: 35 with memfd.
pub(super) fn auth_version(body: &[u8]) -> Result<u16, Malformed> {
    Ok(protocol_version(Tags::new(body).u32()?))
}

/// The default sink's name in a server info reply; sample `t "pulse\0" t "15\0" t "me\0" t "box\0" a 03 02 0000bb80 t "bluez_output.02_00_00_00_00_01.1\0"`.
pub(super) fn default_sink(body: &[u8]) -> Result<Option<&[u8]>, Malformed> {
    let mut tags = Tags::new(body);
    tags.skip_n(BEFORE_DEFAULT_SINK)?;
    Ok(tags.string()?.map(CStr::to_bytes))
}

/// Client list at protocol 35, sample `L 0 t "Recorder\0" L ffffffff N P t "session.fast-capture-start\0" L 00000005 x 00000005 "true\0" N`.
pub(super) fn wants_fast_capture(body: &[u8]) -> Result<bool, Malformed> {
    let mut tags = Tags::new(body);
    let mut hint = false;
    for _ in 0..MAX_CLIENTS {
        if tags.is_empty() {
            break;
        }
        tags.skip_n(BEFORE_CLIENT_PROPERTIES)?;
        hint |= tags.property(FAST_CAPTURE)? == Some(b"true");
    }
    Ok(tags.is_empty() && hint)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Cards {
    pub(super) headset: bool,
    pub(super) device_id: Option<u32>,
}

/// Card list at protocol 35, sample `L 0 t "card\0" L ffffffff N L 0 t "headset-head-unit\0" P N L 0`.
pub(super) fn cards(body: &[u8], card: &[u8], device: BdAddr) -> Result<Cards, Malformed> {
    let mut tags = Tags::new(body);
    let mut cards = Cards::default();
    while !tags.is_empty() {
        let found = read_card(&mut tags, card, device)?;
        cards.headset |= found.headset;
        cards.device_id = found.device_id.or(cards.device_id);
    }
    Ok(cards)
}

/// One card including its profiles and ports, sample `L 0 t "card\0" L ffffffff N L 1 t "off\0" N L 0 L 0 L 0 L 1 t "off\0" P N L 0`.
fn read_card(tags: &mut Tags<'_>, card: &[u8], device: BdAddr) -> Result<Cards, Malformed> {
    tags.u32()?;
    let name = tags.string()?;
    tags.skip_n(BEFORE_CARD_PROFILES)?;
    for _ in 0..tags.u32()? {
        tags.skip_n(PROFILE_FIELDS)?;
    }
    let profile = tags.string()?;
    let mut address = None;
    let mut device_id = None;
    // PipeWire card properties, sample `api.bluez5.address=02:00:00:00:00:01` and `object.id=86`.
    tags.properties(|key, _, value| {
        let value = value.strip_suffix(&[0]).unwrap_or(value);
        match key.to_bytes() {
            b"api.bluez5.address" => {
                address = std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.parse().ok());
            }
            b"object.id" => {
                device_id = std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.parse().ok());
            }
            _ => {}
        }
        Ok(())
    })?;
    for _ in 0..tags.u32()? {
        tags.skip_n(BEFORE_PORT_PROFILES)?;
        for _ in 0..tags.u32()? {
            tags.string()?;
        }
        tags.skip_n(AFTER_PORT_PROFILES)?;
    }
    Ok(Cards {
        headset: name.map(CStr::to_bytes) == Some(card)
            && profile.is_some_and(|name| name.to_bytes().starts_with(HEADSET_PREFIX)),
        device_id: if address == Some(device) {
            device_id
        } else {
            None
        },
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Sink<'a> {
    pub(super) index: u32,
    pub(super) name: &'a [u8],
    pub(super) volume: Percent,
    pub(super) channels: ChannelVolume,
    pub(super) monitor_index: u32,
    pub(super) state: SinkState,
}

/// The sinks of a list reply to its end, the first malformed one ending the walk; sample: one `Tags::sink` after another.
pub(super) fn sinks(body: &[u8]) -> impl Iterator<Item = Result<Sink<'_>, Malformed>> {
    let mut tags = Tags::new(body);
    iter::from_fn(move || {
        if tags.is_empty() {
            return None;
        }
        let sink = tags.sink();
        if sink.is_err() {
            tags = Tags::new(&[]);
        }
        Some(sink)
    })
}

impl<'a> Tags<'a> {
    /// The channels' mean, half up, a boosted sink reading full; sample `v 02 00008000 00010000`: 75.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "the checked channel count is positive and a u32 times 100 fits in u64"
    )]
    fn volume(&mut self) -> Result<(Percent, ChannelVolume), Malformed> {
        let (volumes, sum) = self.channels()?;
        let channels = u64::try_from(volumes.channels().len()).map_err(|_error| TagError::Cut)?;
        let norm = u64::from(Volume::NORM.as_u32());
        let percent = (sum / channels * FULL + norm / 2) / norm;
        let percent = u8::try_from(percent)
            .ok()
            .and_then(Percent::new)
            .unwrap_or(Percent::FULL);
        Ok((percent, volumes))
    }

    /// A sink's state, any number past the protocol's three reading as running, which keeps the gate shut; sample `L 00000002`: suspended.
    fn sink_state(&mut self) -> Result<SinkState, Malformed> {
        Ok(match self.u32()? {
            value if value == protocol::SinkState::Idle.number() => SinkState::Idle,
            value if value == protocol::SinkState::Suspended.number() => SinkState::Suspended,
            _ => SinkState::Running,
        })
    }

    /// Sample: `P t "node.name\0" L 00000004 x 00000004 "mpv\0" N` gives `mpv` under `node.name`.
    pub(super) fn property(&mut self, key: &[u8]) -> Result<Option<&'a [u8]>, Malformed> {
        let mut found = None;
        self.properties(|name, _, value| {
            if name.to_bytes() == key {
                found = Some(value.strip_suffix(&[0]).unwrap_or(value));
            }
            Ok(())
        })?;
        Ok(found)
    }

    /// One sink of a list at protocol 35; sample `L 0 t "bluez_output.02_00_00_00_00_01.1\0" N a m L v 02 8000.. 8000.. 0 L N U N L P N U V L L L L 0 N B 0`.
    fn sink(&mut self) -> Result<Sink<'a>, Malformed> {
        let index = self.u32()?;
        let name = self.string()?.ok_or(Malformed::Nameless)?;
        self.skip_n(BEFORE_VOLUME)?;
        let (volume, channels) = self.volume()?;
        self.skip()?;
        let monitor_index = self.u32()?;
        self.skip_n(BEFORE_STATE)?;
        let state = self.sink_state()?;
        self.skip_n(AFTER_STATE)?;
        for _ in 0..self.u32()? {
            self.skip_n(PORT_FIELDS)?;
        }
        self.skip()?;
        let formats = self.u8()?;
        self.skip_n(usize::from(formats))?;
        Ok(Sink {
            index,
            name: name.to_bytes(),
            volume,
            channels,
            monitor_index,
            state,
        })
    }
}

#[cfg(test)]
pub(super) mod tests;
