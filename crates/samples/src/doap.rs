//! Sample packets of the dictation service on the attribute channel: one firmware's table and the layouts Apple's host software writes, with synthetic contents.

pub mod host;

pub const ERROR_RESPONSE: u8 = 0x01;
pub const FIND_INFORMATION_REQUEST: u8 = 0x04;
const FIND_INFORMATION_RESPONSE: u8 = 0x05;
pub const READ_BY_TYPE_REQUEST: u8 = 0x08;
const READ_BY_TYPE_RESPONSE: u8 = 0x09;
pub const READ_REQUEST: u8 = 0x0a;
const READ_RESPONSE: u8 = 0x0b;
pub const WRITE_REQUEST: u8 = 0x12;
const WRITE_RESPONSE: u8 = 0x13;
const NOTIFICATION: u8 = 0x1b;
const INDICATION: u8 = 0x1d;
const CONFIRMATION: u8 = 0x1e;
const WRITE_COMMAND: u8 = 0x52;

/// Error codes (Bluetooth Core Specification 6.1, Vol 3, Part F, 3.4.1.1).
pub const READ_NOT_PERMITTED: u8 = 0x02;
pub const WRITE_NOT_PERMITTED: u8 = 0x03;
pub const REQUEST_NOT_SUPPORTED: u8 = 0x06;
pub const ATTRIBUTE_NOT_FOUND: u8 = 0x0a;

/// The type of a characteristic declaration, and of the descriptor that turns its notifications on.
pub const DECLARATION: u16 = 0x2803;
pub const CONFIGURATION: u16 = 0x2902;
/// The format byte of a type listing: 16 bit types, or 128 bit ones.
const FORMAT_SHORT: u8 = 0x01;
const FORMAT_LONG: u8 = 0x02;
/// One declaration with a 128 bit identifier: its handle, the property byte, the value handle and the identifier.
const LONG_DECLARATION_LEN: u8 = 21;
/// The descriptor value that turns notifications on.
const NOTIFY: [u8; 2] = [0x01, 0x00];
/// The stream that carries dictation audio, as every value of the service names it.
const STREAM: [u8; 2] = [0x01, 0x00];
/// Opus, the only codec of the stream.
const OPUS: u8 = 0x07;
/// A codec list or a codec choice names one stream.
const ONE_STREAM: u8 = 0x01;
/// What the list holds of the stream behind its identifier: four bytes, which are the codec, a count of two attribute bytes, and those.
const CODEC_ENTRY_LEN: u8 = 0x04;
const CODEC_ATTRIBUTES_LEN: u8 = 0x02;
/// The two attribute bytes one firmware lists for Opus; no host reads what they mean.
const CODEC_ATTRIBUTES: [u8; 2] = [0x01, 0x01];
/// The MTU the pods' own request names, the default of the attribute protocol.
const DEFAULT_MTU: u16 = 23;

/// The reason of a stop a host asks for.
pub const REASON_HOST: u8 = 0x00;
/// The reason the pods refuse a start with, and a host gives for audio nobody asked for.
pub const REASON_UNASKED: u8 = 0x06;

const EVENT_SCORE: u8 = 0x02;
const EVENT_END: u8 = 0x03;
/// The score of a start a host asked for, -1 as a float, and four bytes nothing reads.
const SCORE_DATA: [u8; 8] = [0x00, 0x00, 0x80, 0xbf, 0x00, 0x00, 0x00, 0x00];
/// The flag and the level every sample frame carries.
const FRAME_FLAG: u8 = 0x80;
const FRAME_LEVEL: u8 = 0xd5;

// corner: assumed until measured on hardware, the property bytes; no host reads them
const READS: u8 = 0x02;
const TAKES_COMMANDS: u8 = 0x04;
const NOTIFIES: u8 = 0x10;

/// Supported Codec, Select Codec, Start, Stop, Send Data and Event Indicator, as a UUID is written.
const UUIDS: [u128; 6] = [
    0xf195_b8fb_a9e2_4401_858b_2f87_a069_28a8,
    0xe1f9_b835_7e47_413d_af94_c68e_574b_8f7e,
    0xa08c_e5ef_698a_42a2_b980_7f3a_c00b_3845,
    0x6288_ea2d_7b89_47ad_890b_9fa6_bf3c_fc58,
    0x3f1c_161d_6473_4746_91f5_6d27_6107_80c6,
    0xc7c6_947d_3165_4bcb_8eaf_b328_896c_b531,
];

/// The answer to a read of the codec list: one stream, its identifier, and Opus with two attribute bytes.
pub const CODECS: [u8; 9] = [
    READ_RESPONSE,
    ONE_STREAM,
    STREAM[0],
    STREAM[1],
    CODEC_ENTRY_LEN,
    OPUS,
    CODEC_ATTRIBUTES_LEN,
    CODEC_ATTRIBUTES[0],
    CODEC_ATTRIBUTES[1],
];
/// The answer to a write request.
pub const WRITTEN: [u8; 1] = [WRITE_RESPONSE];
/// An indication of a handle no table here holds, with a value nothing reads.
pub const UNASKED_INDICATION: [u8; 7] = [INDICATION, 0x40, 0x00, 0x01, 0x00, 0xff, 0xff];

/// Where a table holds the six values of the service and the descriptors of the four that notify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handles {
    pub supported_codecs: u16,
    pub select_codec: u16,
    pub start: u16,
    pub stop: u16,
    pub send_data: u16,
    pub event: u16,
    /// The descriptors of `start`, `stop`, `send_data` and `event`, in that order.
    pub cccds: [u16; 4],
}

/// One characteristic as its declaration lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declared {
    pub at: u16,
    pub properties: u8,
    pub value: u16,
    /// The identifier in wire order.
    pub uuid: [u8; 16],
}

impl Handles {
    /// The table of the one firmware seen.
    pub const OBSERVED: Self = Self {
        supported_codecs: 0x0003,
        select_codec: 0x0005,
        start: 0x0007,
        stop: 0x000a,
        send_data: 0x000d,
        event: 0x0010,
        cccds: [0x0008, 0x000b, 0x000e, 0x0011],
    };

    /// The same table with every handle `by` higher.
    #[must_use]
    pub fn shifted(self, by: u16) -> Self {
        let up = |handle: u16| handle.wrapping_add(by);
        Self {
            supported_codecs: up(self.supported_codecs),
            select_codec: up(self.select_codec),
            start: up(self.start),
            stop: up(self.stop),
            send_data: up(self.send_data),
            event: up(self.event),
            cccds: self.cccds.map(up),
        }
    }

    /// The six characteristics in rising order, each declared one handle before its value.
    #[must_use]
    pub fn declared(&self) -> [Declared; 6] {
        let values = [
            (self.supported_codecs, READS),
            (self.select_codec, TAKES_COMMANDS),
            (self.start, TAKES_COMMANDS | NOTIFIES),
            (self.stop, TAKES_COMMANDS | NOTIFIES),
            (self.send_data, NOTIFIES),
            (self.event, TAKES_COMMANDS | NOTIFIES),
        ];
        let mut uuids = UUIDS.into_iter();
        values.map(|(value, properties)| Declared {
            at: value.wrapping_sub(1),
            properties,
            value,
            uuid: uuids.next().unwrap_or_default().to_le_bytes(),
        })
    }
}

/// `09 15`, then `06 00 14 07 00` and sixteen identifier bytes for each.
#[must_use]
pub fn declarations(listed: &[Declared]) -> Vec<u8> {
    let mut reply = vec![READ_BY_TYPE_RESPONSE, LONG_DECLARATION_LEN];
    for declared in listed {
        reply.extend(declared.at.to_le_bytes());
        reply.push(declared.properties);
        reply.extend(declared.value.to_le_bytes());
        reply.extend(declared.uuid);
    }
    reply
}

/// `05 01 08 00 02 29 09 00 03 28`: each handle with its 16 bit type.
#[must_use]
pub fn short_types(listed: &[(u16, u16)]) -> Vec<u8> {
    let mut reply = vec![FIND_INFORMATION_RESPONSE, FORMAT_SHORT];
    for (handle, kind) in listed {
        reply.extend(handle.to_le_bytes());
        reply.extend(kind.to_le_bytes());
    }
    reply
}

/// `05 02 0a 00` and sixteen identifier bytes: each handle with its 128 bit type.
#[must_use]
pub fn long_types(listed: &[(u16, [u8; 16])]) -> Vec<u8> {
    let mut reply = vec![FIND_INFORMATION_RESPONSE, FORMAT_LONG];
    for (handle, uuid) in listed {
        reply.extend(handle.to_le_bytes());
        reply.extend(uuid);
    }
    reply
}

/// `01 08 12 00 0a`: the request's opcode, the handle it named and why it failed.
#[must_use]
pub fn error(request: u8, handle: u16, code: u8) -> [u8; 5] {
    let [low, high] = handle.to_le_bytes();
    [ERROR_RESPONSE, request, low, high, code]
}

fn event(handles: &Handles, code: u8, data: &[u8]) -> Vec<u8> {
    let [low, high] = handles.event.to_le_bytes();
    let len = u8::try_from(data.len()).unwrap_or(u8::MAX);
    let head = [NOTIFICATION, low, high];
    [&head[..], &STREAM, &[code, len], data].concat()
}

/// `1b 10 00 01 00 02 08 00 00 80 bf 00 00 00 00`: the answer to a start, a score of -1.
#[must_use]
pub fn score(handles: &Handles) -> Vec<u8> {
    event(handles, EVENT_SCORE, &SCORE_DATA)
}

/// `1b 10 00 01 00 03 00`: the stream has ended.
#[must_use]
pub fn end(handles: &Handles) -> Vec<u8> {
    event(handles, EVENT_END, &[])
}

/// `1b 0a 00 01 00 06`: the pods stopped or refused the stream, and why.
#[must_use]
pub fn stopped(handles: &Handles, reason: u8) -> [u8; 6] {
    let [low, high] = handles.stop.to_le_bytes();
    let [stream_low, stream_high] = STREAM;
    [NOTIFICATION, low, high, stream_low, stream_high, reason]
}

/// `1b 0d 00 01 00 2a 08 00`, then `80 d5 04 01 03 b8 10 11` for each frame counter and its Opus bytes; a frame is cut to the 255 bytes its length holds.
#[must_use]
pub fn audio(handles: &Handles, packet_counter: u8, frames: &[(u16, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (counter, frame) in frames {
        let len = u8::try_from(frame.len()).unwrap_or(u8::MAX);
        body.extend([FRAME_FLAG, FRAME_LEVEL]);
        body.extend(counter.to_le_bytes());
        body.push(len);
        body.extend(frame.iter().take(usize::from(len)));
    }
    let declared = u16::try_from(body.len()).unwrap_or(u16::MAX);
    let mut pdu = vec![NOTIFICATION];
    pdu.extend(handles.send_data.to_le_bytes());
    pdu.extend(STREAM);
    pdu.push(packet_counter);
    pdu.extend(declared.to_le_bytes());
    pdu.extend(body);
    pdu
}

/// `02 17 00`: a request of the pods' own with two bytes nothing reads, an MTU of 23 under that opcode.
#[must_use]
pub fn request(opcode: u8) -> [u8; 3] {
    let [low, high] = DEFAULT_MTU.to_le_bytes();
    [opcode, low, high]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFT: u16 = 0x20;

    #[test]
    fn the_shifted_table_stands_32_handles_up() {
        let shifted = Handles::OBSERVED.shifted(SHIFT);
        assert_eq!(
            shifted,
            Handles {
                supported_codecs: 0x0023,
                select_codec: 0x0025,
                start: 0x0027,
                stop: 0x002a,
                send_data: 0x002d,
                event: 0x0030,
                cccds: [0x0028, 0x002b, 0x002e, 0x0031],
            }
        );
    }

    #[test]
    fn a_declaration_stands_one_handle_before_its_value() {
        let declared = Handles::OBSERVED.declared();
        let at: Vec<(u16, u16, u8)> = declared
            .iter()
            .map(|declared| (declared.at, declared.value, declared.properties))
            .collect();
        assert_eq!(
            at,
            [
                (0x0002, 0x0003, 0x02),
                (0x0004, 0x0005, 0x04),
                (0x0006, 0x0007, 0x14),
                (0x0009, 0x000a, 0x14),
                (0x000c, 0x000d, 0x10),
                (0x000f, 0x0010, 0x14),
            ]
        );
        // The Start identifier a08ce5ef-698a-42a2-b980-7f3ac00b3845, least significant byte first.
        assert_eq!(
            declared[2].uuid,
            [
                0x45, 0x38, 0x0b, 0xc0, 0x3a, 0x7f, 0x80, 0xb9, 0xa2, 0x42, 0x8a, 0x69, 0xef, 0xe5,
                0x8c, 0xa0
            ]
        );
        let reply = declarations(&declared[2..3]);
        assert_eq!(reply[..7], [0x09, 0x15, 0x06, 0x00, 0x14, 0x07, 0x00]);
        assert_eq!(reply[7..], declared[2].uuid);
    }

    #[test]
    fn the_listings_and_the_answers_are_the_documented_bytes() {
        assert_eq!(
            short_types(&[(0x0008, CONFIGURATION), (0x0009, DECLARATION)]),
            [0x05, 0x01, 0x08, 0x00, 0x02, 0x29, 0x09, 0x00, 0x03, 0x28]
        );
        let long = long_types(&[(0x000a, [0xee; 16])]);
        assert_eq!(long[..4], [0x05, 0x02, 0x0a, 0x00]);
        assert_eq!(long[4..], [0xee; 16]);
        assert_eq!(
            error(READ_BY_TYPE_REQUEST, 0x0012, ATTRIBUTE_NOT_FOUND),
            [0x01, 0x08, 0x12, 0x00, 0x0a]
        );
        assert_eq!(
            error(FIND_INFORMATION_REQUEST, 0x0012, ATTRIBUTE_NOT_FOUND)[1],
            0x04
        );
        assert_eq!(
            error(READ_REQUEST, 0x0005, READ_NOT_PERMITTED)[1..],
            [0x0a, 0x05, 0x00, 0x02]
        );
        assert_eq!(
            error(WRITE_REQUEST, 0x0003, WRITE_NOT_PERMITTED)[1..],
            [0x12, 0x03, 0x00, 0x03]
        );
        assert_eq!(WRITTEN, [0x13]);
        assert_eq!(
            CODECS,
            [0x0b, 0x01, 0x01, 0x00, 0x04, 0x07, 0x02, 0x01, 0x01]
        );
        assert_eq!(UNASKED_INDICATION[0], 0x1d);
        assert_eq!(request(0x02), [0x02, 0x17, 0x00]);
    }

    #[test]
    fn the_notifications_are_the_bytes_seen_at_the_observed_handles() {
        let table = Handles::OBSERVED;
        assert_eq!(
            score(&table),
            [
                0x1b, 0x10, 0x00, 0x01, 0x00, 0x02, 0x08, 0x00, 0x00, 0x80, 0xbf, 0x00, 0x00, 0x00,
                0x00
            ]
        );
        assert_eq!(end(&table), [0x1b, 0x10, 0x00, 0x01, 0x00, 0x03, 0x00]);
        assert_eq!(
            stopped(&table, REASON_UNASKED),
            [0x1b, 0x0a, 0x00, 0x01, 0x00, 0x06]
        );
        assert_eq!(
            audio(&table, 0x2a, &[(0x0104, &[0xb8, 0x10, 0x11])]),
            [
                0x1b, 0x0d, 0x00, 0x01, 0x00, 0x2a, 0x08, 0x00, 0x80, 0xd5, 0x04, 0x01, 0x03, 0xb8,
                0x10, 0x11
            ]
        );
        assert_eq!(score(&table.shifted(SHIFT))[1..3], [0x30, 0x00]);
    }

    #[test]
    fn a_frame_longer_than_its_length_byte_is_cut_to_it() {
        let pdu = audio(&Handles::OBSERVED, 0, &[(0, &[0xb8; 300])]);
        assert_eq!(pdu[6..8], [0x04, 0x01]);
        assert_eq!(pdu[12], 0xff);
        assert_eq!(pdu.len(), 8 + 5 + 255);
    }
}
