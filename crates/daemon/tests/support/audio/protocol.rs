//! Native protocol fixture messages for the in-process and external fake audio servers.

#![forbid(unsafe_code)]

#[path = "protocol/fields.rs"]
mod fields;
#[path = "protocol/replies.rs"]
mod replies;

use std::ffi::CStr;
use std::io::{self, Write};

pub(crate) use super::native::*;
use fields::Reader;
pub(crate) use replies::*;

pub(crate) const MAX_VERSION: u16 = PROTOCOL_VERSION;
pub(crate) const DEFAULT_SINK: &CStr = c"@DEFAULT_SINK@";

#[derive(Debug)]
pub(crate) enum ProtocolError {
    Invalid(&'static str),
    Io(io::Error),
    ServerError(PulseError),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(problem) => f.write_str(problem),
            Self::Io(error) => error.fmt(f),
            Self::ServerError(error) => write!(f, "server refused with {error:?}"),
        }
    }
}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) type Result<T> = std::result::Result<T, ProtocolError>;

// Sample: L 00000014 L 00000001 gives GetServerInfo at sequence 1.
pub(crate) fn command_header(payload: &[u8]) -> Result<(u32, u32)> {
    let mut reader = Reader(payload);
    Ok((reader.u32()?, reader.u32()?))
}

// Sample: L 00000024 L 00000001 L 00000007 N v 01 00010000 sets sink 7 to full volume.
pub(crate) fn read_command(payload: &[u8]) -> Result<(u32, Command)> {
    let mut reader = Reader(payload);
    let number = reader.u32()?;
    let seq = reader.u32()?;
    let command = match number {
        n if n == CommandTag::Auth.number() => {
            let raw = reader.u32()?;
            Command::Auth(AuthParams {
                version: protocol_version(raw),
                supports_shm: raw & FLAG_SHM != 0,
                supports_memfd: raw & FLAG_MEMFD != 0,
                cookie: reader.arbitrary()?.to_vec(),
            })
        }
        n if n == CommandTag::SetClientName.number() => Command::SetClientName(reader.props()?),
        n if n == CommandTag::GetServerInfo.number() => Command::GetServerInfo,
        n if n == CommandTag::GetSinkInfo.number() => Command::GetSinkInfo(reader.device()?),
        n if n == CommandTag::GetSinkInfoList.number() => Command::GetSinkInfoList,
        n if n == CommandTag::GetSourceInfoList.number() => Command::GetSourceInfoList,
        n if n == CommandTag::GetModuleInfoList.number() => Command::GetModuleInfoList,
        n if n == CommandTag::GetClientInfoList.number() => Command::GetClientInfoList,
        n if n == CommandTag::GetSourceOutputInfoList.number() => Command::GetSourceOutputInfoList,
        n if n == CommandTag::Subscribe.number() => {
            Command::Subscribe(SubscriptionMask::from_bits_truncate(reader.u32()?))
        }
        n if n == CommandTag::SetSinkVolume.number() => {
            Command::SetSinkVolume(SetDeviceVolumeParams {
                device_index: reader.index()?,
                device_name: reader.string()?,
                volume: reader.channels()?,
            })
        }
        n if n == CommandTag::SetDefaultSource.number() => {
            Command::SetDefaultSource(reader.name()?)
        }
        n if n == CommandTag::SubscribeEvent.number() => {
            Command::SubscribeEvent(read_event(&mut reader)?)
        }
        n if n == CommandTag::SuspendSink.number() => Command::SuspendSink(SuspendParams {
            device_index: reader.index()?,
            device_name: reader.string()?,
            suspend: reader.boolean()?,
        }),
        n if n == CommandTag::GetCardInfo.number() => Command::GetCardInfo(reader.device()?),
        n if n == CommandTag::GetCardInfoList.number() => Command::GetCardInfoList,
        n if n == CommandTag::SetCardProfile.number() => {
            Command::SetCardProfile(SetCardProfileParams {
                card_index: reader.index()?,
                card_name: reader.string()?,
                profile_name: reader.name()?,
            })
        }
        _ => return Err(ProtocolError::Invalid("unknown PulseAudio command")),
    };
    reader.finish()?;
    Ok((seq, command))
}

// Sample: L 00000010 L 00000007 reports a change to sink 7.
fn read_event(reader: &mut Reader<'_>) -> Result<SubscriptionEvent> {
    let flags = reader.u32()?;
    Ok(SubscriptionEvent {
        event_facility: SubscriptionEventFacility::from_number(flags & FACILITY_MASK)
            .ok_or(ProtocolError::Invalid("invalid PulseAudio event facility"))?,
        event_type: SubscriptionEventType::from_number(flags & !FACILITY_MASK)
            .ok_or(ProtocolError::Invalid("invalid PulseAudio event type"))?,
        index: reader.index()?,
    })
}

// Sample: 00 00 00 0a ff ff ff ff, 12 zero bytes, then ten payload bytes.
fn control_payload(frame: &[u8]) -> Result<&[u8]> {
    let mut payload = frame;
    let descriptor = read_descriptor(&mut payload)?;
    if descriptor.channel != CONTROL_CHANNEL
        || usize::try_from(descriptor.length) != Ok(payload.len())
    {
        return Err(ProtocolError::Invalid(
            "invalid PulseAudio control descriptor",
        ));
    }
    Ok(payload)
}

// Sample: a control frame with payload L 00000042 L ffffffff L 00000010 L 00000007.
pub(crate) fn read_command_message(frame: &[u8]) -> Result<(u32, Command)> {
    read_command(control_payload(frame)?)
}

pub(crate) fn write_descriptor(writer: &mut impl Write, descriptor: Descriptor) -> io::Result<()> {
    writer.write_all(&descriptor.encode())
}

pub(crate) fn write_payload(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    write_descriptor(
        writer,
        Descriptor {
            length: field_length(payload.len())?,
            channel: CONTROL_CHANNEL,
        },
    )?;
    writer.write_all(payload)
}

pub(crate) fn write_command_message(
    writer: &mut impl Write,
    seq: u32,
    command: &Command,
) -> io::Result<()> {
    write_payload(writer, &command.encode(seq)?)
}

fn reply_payload(tag: CommandTag, seq: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    let mut tags = TagWriter(&mut payload);
    tags.u32(tag.number());
    tags.u32(seq);
    payload
}

pub(crate) fn write_ack_message(writer: &mut impl Write, seq: u32) -> io::Result<()> {
    write_payload(writer, &reply_payload(CommandTag::Reply, seq))
}

pub(crate) fn write_error(writer: &mut impl Write, seq: u32, error: PulseError) -> io::Result<()> {
    let mut payload = reply_payload(CommandTag::Error, seq);
    TagWriter(&mut payload).u32(error.number());
    write_payload(writer, &payload)
}

pub(crate) fn write_reply_message(
    writer: &mut impl Write,
    seq: u32,
    reply: &impl CommandReply,
    version: u16,
) -> io::Result<()> {
    let mut payload = reply_payload(CommandTag::Reply, seq);
    reply.write(&mut TagWriter(&mut payload), version)?;
    write_payload(writer, &payload)
}

// Sample: L 00000000 L 00000007 L 00000005 refuses sequence 7 with NoEntity.
fn reply_body(frame: &[u8]) -> Result<(u32, Reader<'_>)> {
    let mut reader = Reader(control_payload(frame)?);
    let command = reader.u32()?;
    let seq = reader.u32()?;
    if command == CommandTag::Reply.number() {
        return Ok((seq, reader));
    }
    if command == CommandTag::Error.number() {
        let error = match reader.u32()? {
            n if n == PulseError::AccessDenied.number() => PulseError::AccessDenied,
            n if n == PulseError::Command.number() => PulseError::Command,
            n if n == PulseError::Invalid.number() => PulseError::Invalid,
            n if n == PulseError::NoEntity.number() => PulseError::NoEntity,
            _ => return Err(ProtocolError::Invalid("unknown PulseAudio refusal")),
        };
        reader.finish()?;
        return Err(ProtocolError::ServerError(error));
    }
    Err(ProtocolError::Invalid("expected a PulseAudio reply"))
}

// Sample: an ack has payload L 00000002 L 00000007 and no body.
pub(crate) fn read_ack_message(frame: &[u8]) -> Result<u32> {
    let (seq, reader) = reply_body(frame)?;
    reader.finish()?;
    Ok(seq)
}

// Sample: L 00000002 L 00000007 L 00000023 is an auth reply granting version 35.
pub(crate) fn read_reply_message<R: CommandReply>(frame: &[u8], version: u16) -> Result<(u32, R)> {
    let (seq, mut reader) = reply_body(frame)?;
    let value = R::read(&mut reader, version)?;
    reader.finish()?;
    Ok((seq, value))
}
