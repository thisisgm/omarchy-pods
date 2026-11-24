//! Frames of the audio server's native protocol: requests written through the `pulseaudio` crate, the server's bytes read here.

use std::fmt;
use std::io::{self, ErrorKind};
use std::iter;

use pods_core::policy::audio::{Percent, SinkState};
use pulseaudio::protocol::{
    Command, DESCRIPTOR_SIZE, Descriptor, DescriptorFlags, Volume, encode_descriptor,
    read_descriptor,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// A card list is a few kilobytes; a larger frame ends the connection.
pub(super) const AUDIO_FRAME_MAX: usize = 262_144;

/// The version whose sink layout `Tags::sink` walks; `PipeWire`'s pulse server and `PulseAudio` 15 onward speak it.
pub(super) const PROTOCOL_VERSION: u16 = 35;

/// Command frames travel on this channel; any other carries stream audio, which this client never opens.
const CONTROL_CHANNEL: u32 = u32::MAX;

/// Format, channel count and a 4 byte rate.
const SAMPLE_SPEC_LEN: usize = 6;

/// Server name and version, user name, host name and sample spec, before the default sink.
const BEFORE_DEFAULT_SINK: usize = 5;

/// Description, sample spec, channel map and owner module, between a sink's name and its volume.
const BEFORE_VOLUME: usize = 4;

/// Mute, monitor index and name, two latencies, driver, flags, properties and base volume.
const BEFORE_STATE: usize = 9;

/// Volume steps and card.
const AFTER_STATE: usize = 2;

/// Name, description, priority, availability, availability group and type.
const PORT_FIELDS: usize = 6;

/// The percent that `Volume::NORM` stands for.
const FULL: u64 = 100;

/// The crate's tag bytes, so the table is written once.
#[expect(
    clippy::as_conversions,
    reason = "the discriminant of a fieldless enum is the protocol's tag byte"
)]
mod tag {
    use pulseaudio::protocol::Tag;

    pub(super) const STRING: u8 = Tag::String as u8;
    pub(super) const STRING_NULL: u8 = Tag::StringNull as u8;
    pub(super) const U32: u8 = Tag::U32 as u8;
    pub(super) const U8: u8 = Tag::U8 as u8;
    pub(super) const U64: u8 = Tag::U64 as u8;
    pub(super) const S64: u8 = Tag::S64 as u8;
    pub(super) const SAMPLE_SPEC: u8 = Tag::SampleSpec as u8;
    pub(super) const ARBITRARY: u8 = Tag::Arbitrary as u8;
    pub(super) const TRUE: u8 = Tag::BooleanTrue as u8;
    pub(super) const FALSE: u8 = Tag::BooleanFalse as u8;
    pub(super) const TIMEVAL: u8 = Tag::TimeVal as u8;
    pub(super) const USEC: u8 = Tag::Usec as u8;
    pub(super) const CHANNEL_MAP: u8 = Tag::ChannelMap as u8;
    pub(super) const CVOLUME: u8 = Tag::CVolume as u8;
    pub(super) const PROPLIST: u8 = Tag::PropList as u8;
    pub(super) const VOLUME: u8 = Tag::Volume as u8;
    pub(super) const FORMAT_INFO: u8 = Tag::FormatInfo as u8;
}

/// The crate's numbers for the two sink states the gate tells apart from running.
#[expect(
    clippy::as_conversions,
    reason = "the discriminant of a fieldless enum is the protocol's state number"
)]
mod state {
    use pulseaudio::protocol::SinkState;

    pub(super) const IDLE: u32 = SinkState::Idle as u32;
    pub(super) const SUSPENDED: u32 = SinkState::Suspended as u32;
}

/// The crate's numbers for the three commands a server sends this client.
#[expect(
    clippy::as_conversions,
    reason = "the discriminant of a fieldless enum is the protocol's command number"
)]
mod command {
    use pulseaudio::protocol::CommandTag;

    pub(super) const ERROR: u32 = CommandTag::Error as u32;
    pub(super) const REPLY: u32 = CommandTag::Reply as u32;
    pub(super) const EVENT: u32 = CommandTag::SubscribeEvent as u32;
}

/// What is wrong with the server's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Malformed {
    Oversized(u32),
    StreamData(u32),
    Cut,
    Tag(u8),
    Nameless,
    NoChannels,
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversized(length) => {
                write!(f, "a frame of {length} bytes, past {AUDIO_FRAME_MAX}")
            }
            Self::StreamData(channel) => write!(f, "stream data on channel {channel}"),
            Self::Cut => f.write_str("a value cut short"),
            Self::Tag(tag) => write!(f, "an unexpected tag 0x{tag:02x}"),
            Self::Nameless => f.write_str("a sink with no name"),
            Self::NoChannels => f.write_str("a volume of no channels"),
        }
    }
}

/// One message's payload; its descriptor is stripped on read and made on write.
#[derive(Debug)]
pub(super) struct Frame(Vec<u8>);

impl Frame {
    pub(super) fn request(seq: u32, command: &Command) -> io::Result<Self> {
        let mut payload = Vec::new();
        command
            .write_tag_prefixed(seq, &mut payload, PROTOCOL_VERSION)
            .map_err(io::Error::other)?;
        Ok(Self(payload))
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
fn take_frame(pending: &mut Vec<u8>) -> io::Result<Option<Frame>> {
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
    let length = u32::try_from(frame.0.len()).map_err(io::Error::other)?;
    let mut descriptor = [0; DESCRIPTOR_SIZE];
    let control = Descriptor {
        length,
        channel: CONTROL_CHANNEL,
        offset: 0,
        flags: DescriptorFlags::empty(),
    };
    encode_descriptor(&mut descriptor, &control);
    stream.write_all(&descriptor).await?;
    stream.write_all(&frame.0).await
}

/// The payload length a descriptor declares; sample `00 00 00 0a ff ff ff ff` then 12 zero bytes: 10 bytes of commands.
fn payload_length(descriptor: &[u8; DESCRIPTOR_SIZE]) -> Result<usize, Malformed> {
    let Descriptor {
        length, channel, ..
    } = read_descriptor(&mut descriptor.as_slice())
        .ok()
        .ok_or(Malformed::Cut)?;
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
    Event,
    /// A command this client never asks about, as a stream's.
    Other,
}

/// A frame's command, its sequence and what follows; sample `L 00000002 L 00000005` then the values: the answer to request 5.
pub(super) fn message(frame: &Frame) -> Result<Message<'_>, Malformed> {
    let mut tags = Tags(&frame.0);
    let number = tags.u32()?;
    let seq = tags.u32()?;
    Ok(match number {
        command::REPLY => Message::Reply { seq, body: tags.0 },
        command::ERROR => Message::Refused {
            seq,
            code: tags.u32()?,
        },
        command::EVENT => Message::Event,
        _ => Message::Other,
    })
}

/// The version an auth reply grants, its high half being memory flags; sample `L 40 00 00 23`: 35 with memfd.
pub(super) fn auth_version(body: &[u8]) -> Result<u16, Malformed> {
    let [_, _, high, low] = Tags(body).u32()?.to_be_bytes();
    Ok(u16::from_be_bytes([high, low]))
}

/// The default sink's name in a server info reply; sample `t "pulse\0" t "15\0" t "me\0" t "box\0" a 03 02 0000bb80 t "bluez_output.02_00_00_00_00_01.1\0"`.
pub(super) fn default_sink(body: &[u8]) -> Result<Option<&[u8]>, Malformed> {
    let mut tags = Tags(body);
    tags.skip_n(BEFORE_DEFAULT_SINK)?;
    tags.string()
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Sink<'a> {
    pub(super) index: u32,
    pub(super) name: &'a [u8],
    pub(super) volume: Percent,
    pub(super) state: SinkState,
}

/// The sinks of a list reply to its end, the first malformed one ending the walk; sample: one `Tags::sink` after another.
pub(super) fn sinks(body: &[u8]) -> impl Iterator<Item = Result<Sink<'_>, Malformed>> {
    let mut tags = Tags(body);
    iter::from_fn(move || {
        if tags.0.is_empty() {
            return None;
        }
        let sink = tags.sink();
        if sink.is_err() {
            tags = Tags(&[]);
        }
        Some(sink)
    })
}

/// A walk over a tagstruct, copying nothing: the crate's reader allocates a property's declared length, up to 4 GiB, before checking it.
pub(super) struct Tags<'a>(pub(super) &'a [u8]);

impl<'a> Tags<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Malformed> {
        let (taken, rest) = self.0.split_at_checked(count).ok_or(Malformed::Cut)?;
        self.0 = rest;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8, Malformed> {
        let (&byte, rest) = self.0.split_first().ok_or(Malformed::Cut)?;
        self.0 = rest;
        Ok(byte)
    }

    fn raw_u32(&mut self) -> Result<u32, Malformed> {
        let (&bytes, rest) = self.0.split_first_chunk().ok_or(Malformed::Cut)?;
        self.0 = rest;
        Ok(u32::from_be_bytes(bytes))
    }

    fn expect(&mut self, wanted: u8) -> Result<(), Malformed> {
        match self.byte()? {
            got if got == wanted => Ok(()),
            got => Err(Malformed::Tag(got)),
        }
    }

    pub(super) fn u32(&mut self) -> Result<u32, Malformed> {
        self.expect(tag::U32)?;
        self.raw_u32()
    }

    fn u8(&mut self) -> Result<u8, Malformed> {
        self.expect(tag::U8)?;
        self.byte()
    }

    /// Without its closing NUL, a null string being `None`; sample `t "pulse\0"` or `N`.
    fn string(&mut self) -> Result<Option<&'a [u8]>, Malformed> {
        match self.byte()? {
            tag::STRING_NULL => Ok(None),
            tag::STRING => self.c_string().map(Some),
            got => Err(Malformed::Tag(got)),
        }
    }

    /// The bytes to the first NUL, which is consumed; sample `pulse\0`.
    fn c_string(&mut self) -> Result<&'a [u8], Malformed> {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(Malformed::Cut)?;
        let text = self.take(end)?;
        self.take(1)?;
        Ok(text)
    }

    /// A length then that many bytes; sample `0000000b "Audio/Sink\0"`.
    fn arbitrary(&mut self) -> Result<&'a [u8], Malformed> {
        let length = self.raw_u32()?;
        // corner: a length past the address space is past the frame too, so the take fails
        self.take(usize::try_from(length).unwrap_or(usize::MAX))
    }

    /// The channels' mean, half up, a boosted sink reading full; sample `v 02 00008000 00010000`: 75.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "255 channels of a u32 sum in a u64, a u32 times 100 fits a u64, and neither divisor is zero"
    )]
    fn volume(&mut self) -> Result<Percent, Malformed> {
        self.expect(tag::CVOLUME)?;
        let channels = self.byte()?;
        if channels == 0 {
            return Err(Malformed::NoChannels);
        }
        let mut sum = 0_u64;
        for _ in 0..channels {
            sum += u64::from(self.raw_u32()?);
        }
        let norm = u64::from(Volume::NORM.as_u32());
        let percent = (sum / u64::from(channels) * FULL + norm / 2) / norm;
        Ok(u8::try_from(percent)
            .ok()
            .and_then(Percent::new)
            .unwrap_or(Percent::FULL))
    }

    /// A sink's state, any number past the protocol's three reading as running, which keeps the gate shut; sample `L 00000002`: suspended.
    fn sink_state(&mut self) -> Result<SinkState, Malformed> {
        Ok(match self.u32()? {
            state::IDLE => SinkState::Idle,
            state::SUSPENDED => SinkState::Suspended,
            _ => SinkState::Running,
        })
    }

    /// Key, length and value triples to a null key, each key and value shown to `see`; sample `t "media.class\0" L 0000000b x 0000000b "Audio/Sink\0" N`.
    fn proplist(&mut self, mut see: impl FnMut(&'a [u8], &'a [u8])) -> Result<(), Malformed> {
        while let Some(key) = self.string()? {
            self.u32()?;
            self.expect(tag::ARBITRARY)?;
            see(key, self.arbitrary()?);
        }
        Ok(())
    }

    /// A property list's value under `key`, without its closing NUL; sample `P t "node.name\0" L 00000004 x 00000004 "mpv\0" N` under `node.name`: `mpv`.
    pub(super) fn property(&mut self, key: &[u8]) -> Result<Option<&'a [u8]>, Malformed> {
        self.expect(tag::PROPLIST)?;
        let mut found = None;
        self.proplist(|name, value| {
            if name == key {
                found = Some(value.strip_suffix(&[0]).unwrap_or(value));
            }
        })?;
        Ok(found)
    }

    /// One value of any tag, a property list or a format walked flat so no input nests; sample `L 00000005` or `t "name\0"`.
    fn skip(&mut self) -> Result<(), Malformed> {
        let length = match self.byte()? {
            tag::STRING => return self.c_string().map(|_| ()),
            tag::ARBITRARY => return self.arbitrary().map(|_| ()),
            tag::PROPLIST => return self.proplist(|_, _| {}),
            tag::FORMAT_INFO => {
                self.u8()?;
                self.expect(tag::PROPLIST)?;
                return self.proplist(|_, _| {});
            }
            tag::STRING_NULL | tag::TRUE | tag::FALSE => 0,
            tag::U8 => size_of::<u8>(),
            tag::U32 | tag::VOLUME => size_of::<u32>(),
            tag::U64 | tag::S64 | tag::USEC | tag::TIMEVAL => size_of::<u64>(),
            tag::SAMPLE_SPEC => SAMPLE_SPEC_LEN,
            tag::CHANNEL_MAP => usize::from(self.byte()?),
            tag::CVOLUME => usize::from(self.byte()?).saturating_mul(size_of::<u32>()),
            got => return Err(Malformed::Tag(got)),
        };
        self.take(length).map(|_| ())
    }

    pub(super) fn skip_n(&mut self, count: usize) -> Result<(), Malformed> {
        for _ in 0..count {
            self.skip()?;
        }
        Ok(())
    }

    /// One sink of a list at protocol 35; sample `L 0 t "bluez_output.02_00_00_00_00_01.1\0" N a m L v 02 8000.. 8000.. 0 L N U N L P N U V L L L L 0 N B 0`.
    fn sink(&mut self) -> Result<Sink<'a>, Malformed> {
        let index = self.u32()?;
        let name = self.string()?.ok_or(Malformed::Nameless)?;
        self.skip_n(BEFORE_VOLUME)?;
        let volume = self.volume()?;
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
            name,
            volume,
            state,
        })
    }
}

#[cfg(test)]
pub(super) mod tests;
