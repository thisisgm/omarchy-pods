//! Frames and volumes of the native audio server protocol, read and written only through the `pulseaudio` crate.

use std::io::{self, ErrorKind, Read};

use pods_proto::addr::BdAddr;
use pulseaudio::protocol::{
    Command, CommandReply, DESCRIPTOR_SIZE, Descriptor, DescriptorFlags, PulseError,
    TagStructReader, Volume, read_descriptor, write_ack_message, write_command_message,
    write_descriptor, write_error, write_reply_message,
};

/// The largest frame the fake or its test client reads; a sink list is a few kilobytes.
const FRAME_MAX: u32 = 262_144;

/// Declared by `send_oversized_frame`, past any bound a reader may set.
pub(crate) const OVERSIZED_LENGTH: u32 = u32::MAX;

/// The channel of a command frame; any other number carries stream audio.
pub(super) const CONTROL_CHANNEL: u32 = u32::MAX;

/// The sequence number of an event, which answers no command.
const EVENT_SEQ: u32 = u32::MAX;

/// What `PipeWire` names the output node of a Bluetooth device: `02:00:00:00:00:01` gives `bluez_output.02_00_00_00_00_01.1`.
pub(crate) fn sink_of(address: &str) -> String {
    let octets = address
        .parse::<BdAddr>()
        .unwrap_or_else(|error| panic!("a sink for a bad address: {error}"))
        .octets();
    let groups: Vec<String> = octets.iter().map(|octet| format!("{octet:02X}")).collect();
    format!("bluez_output.{}.1", groups.join("_"))
}

/// The percent that `Volume::NORM` stands for.
const FULL: u32 = 100;

/// Percent as `pactl` reads and prints it, rounded half up.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "255 % of `Volume::NORM`, the largest product, fits a u32"
)]
pub(crate) fn volume_of(percent: u8) -> Volume {
    let norm = Volume::NORM.as_u32();
    Volume::from_u32_clamped((u32::from(percent) * norm + FULL / 2) / FULL)
}

/// The inverse of `volume_of`, saturating at 255 %, past any volume a test writes.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "a u32 volume times 100 fits a u64, and `Volume::NORM` is not zero"
)]
pub(crate) fn percent_of(volume: Volume) -> u8 {
    let norm = u64::from(Volume::NORM.as_u32());
    let percent = (u64::from(volume.as_u32()) * u64::from(FULL) + norm / 2) / norm;
    u8::try_from(percent).unwrap_or(u8::MAX)
}

/// One whole frame, descriptor included; sample `00 00 00 0a ff ff ff ff` then 12 zero bytes, then a 10 byte payload.
pub(crate) fn read_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut frame = vec![0; DESCRIPTOR_SIZE];
    // corner: an end of stream before the first byte is the peer closing between frames
    reader.read_exact(&mut frame[..1])?;
    read_rest(reader, &mut frame[1..])?;
    let descriptor = read_descriptor(&mut frame.as_slice()).map_err(io::Error::other)?;
    if descriptor.length > FRAME_MAX {
        let declared = descriptor.length;
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("a frame of {declared} bytes, past {FRAME_MAX}"),
        ));
    }
    let length = usize::try_from(descriptor.length).map_err(io::Error::other)?;
    frame.resize(DESCRIPTOR_SIZE.saturating_add(length), 0);
    read_rest(reader, &mut frame[DESCRIPTOR_SIZE..])?;
    Ok(frame)
}

/// Fills the rest of a frame, where an end of stream is a frame cut short and not a close.
fn read_rest(reader: &mut impl Read, rest: &mut [u8]) -> io::Result<()> {
    reader.read_exact(rest).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            io::Error::new(ErrorKind::InvalidData, "a frame cut short")
        } else {
            error
        }
    })
}

/// The descriptor's channel and the payload's command number and sequence; sample payload `4c 00 00 00 08 4c 00 00 00 01`.
pub(crate) fn header(frame: &[u8]) -> Result<(u32, u32, u32), String> {
    let descriptor = read_descriptor(&mut &frame[..]).map_err(|error| error.to_string())?;
    let mut payload = &frame[DESCRIPTOR_SIZE..];
    let mut tags = TagStructReader::new(&mut payload, pulseaudio::protocol::MAX_VERSION);
    let command = tags.read_u32().map_err(|error| error.to_string())?;
    let seq = tags.read_u32().map_err(|error| error.to_string())?;
    Ok((descriptor.channel, command, seq))
}

/// The command a frame carries, whole to its last byte as a real server demands; sample payload `4c 00 00 00 14 4c 00 00 00 01`.
pub(crate) fn command(frame: &[u8], version: u16) -> Result<Command, String> {
    let mut payload = &frame[DESCRIPTOR_SIZE..];
    let (_, command) =
        Command::read_tag_prefixed(&mut payload, version).map_err(|error| error.to_string())?;
    if !payload.is_empty() {
        return Err(format!(
            "{} bytes after a {:?}",
            payload.len(),
            command.tag()
        ));
    }
    Ok(command)
}

pub(super) fn reply(seq: u32, reply: &impl CommandReply, version: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_reply_message(&mut bytes, seq, reply, version).unwrap();
    bytes
}

pub(super) fn ack(seq: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_ack_message(&mut bytes, seq).unwrap();
    bytes
}

pub(super) fn error(seq: u32, error: PulseError) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_error(&mut bytes, seq, &error).unwrap();
    bytes
}

pub(super) fn event(event: &Command, version: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_command_message(&mut bytes, EVENT_SEQ, event, version).unwrap();
    bytes
}

/// A descriptor alone, declaring `OVERSIZED_LENGTH` bytes that never follow.
pub(super) fn oversized_frame() -> Vec<u8> {
    let descriptor = Descriptor {
        length: OVERSIZED_LENGTH,
        channel: CONTROL_CHANNEL,
        offset: 0,
        flags: DescriptorFlags::empty(),
    };
    let mut bytes = Vec::new();
    write_descriptor(&mut bytes, &descriptor).unwrap();
    bytes
}
