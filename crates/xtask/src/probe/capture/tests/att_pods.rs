//! What the fake pods do on the attribute channel: they list a table, and stream audio notifications from the start write to the stop write.

use std::time::{Duration, Instant};

use pods_proto::att;
use pods_proto::dictation::{
    AUDIO_HANDLE, Frame, START, START_HANDLE, STARTED, STATUS_HANDLE, STOP, STOP_HANDLE, STOPPED,
    TOC_CELT_WIDEBAND_20MS_MONO, encode_audio,
};

use super::pods::noise;

/// The opcode of a Find Information request.
pub(super) const FIND_INFORMATION: u8 = 0x04;
/// The opcode of a Find Information response.
const FIND_INFORMATION_RESPONSE: u8 = 0x05;
/// The format byte of a Find Information response whose types are 16 bit.
const FORMAT_SHORT: u8 = 0x01;
/// The opcode of a Read By Type request.
pub(super) const READ_BY_TYPE: u8 = 0x08;
/// The opcode of a Read By Type response.
const READ_BY_TYPE_RESPONSE: u8 = 0x09;
/// The opcode of a Write Response, which carries nothing more.
pub(super) const WRITE_RESPONSE: u8 = 0x13;
/// The opcode of a Read Response, which the value follows.
pub(super) const READ_RESPONSE: u8 = 0x0b;
/// The value of every attribute they are asked to read.
pub(super) const READ_VALUE: [u8; 2] = [0x5a, 0xa5];
/// The error code of a refused write (Vol 3, Part F, 3.4.1.1).
pub(super) const WRITE_NOT_PERMITTED: u8 = 0x03;
/// The error code of a refused read.
pub(super) const READ_NOT_PERMITTED: u8 = 0x02;
/// The length of each declaration in a reply: its handle, the property byte, the value handle and a 16 bit type.
const DECLARATION_LEN: u8 = 0x07;
/// How many attributes they list in one reply unless a test says otherwise, so a walk of the table takes several requests.
const PAGE: usize = 3;
/// The last handle of the table they list unless a test says otherwise, past the four the stream uses.
pub(super) const TABLE_END: u16 = 0x0012;
pub(super) const BURST_PACKETS: usize = 13;
/// The frames in each of the thirteen packets a session opens with, 63 in all, as a Mac was seen to get them.
const BURST_FRAMES: [u16; BURST_PACKETS] = [4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 4];
/// How long after the start write the first packet of a paced session leaves.
pub(super) const BURST_AFTER: Duration = Duration::from_millis(80);
/// How long the opening packets of a paced session take from the first to the last, as a Mac was seen to get them.
pub(super) const BURST_SPAN: Duration = Duration::from_millis(250);
const BURST_GAPS: u32 = 12;
/// The codec bytes that follow the table of contents byte in every frame here.
const FRAME_NOISE_LEN: usize = 9;
/// The flag and the level every frame here carries; the loop logs them and reads neither.
const FRAME_PREFIX: (u8, u8) = (0x80, 0xd5);
/// Stands in for the 16 bit type of every attribute and declaration here; the walk reads handles and no types.
pub(super) const MADE_UP_TYPE: [u8; 2] = [0x2a, 0x2b];
const WRITES: u8 = att::PROPERTY_WRITE_WITHOUT_RESPONSE;
const NOTIFIES: u8 = att::PROPERTY_NOTIFY;
pub(super) const DECLARED: [(u16, u8); 4] = [
    (START_HANDLE, WRITES),
    (STOP_HANDLE, WRITES),
    (AUDIO_HANDLE, NOTIFIES),
    (STATUS_HANDLE, NOTIFIES),
];

pub(super) fn declared_at(value: u16) -> u16 {
    value.saturating_sub(1)
}

/// The clock of a paced session: the pods hold `lead` of audio when the start write comes, and no frame leaves before it has ended.
#[derive(Clone, Copy)]
pub(super) struct Pace {
    pub(super) frame: Duration,
    /// How long before the start write the first frame began.
    pub(super) lead: Duration,
}

/// Their part on the attribute channel; `AttPods::default()` lists a table that holds the four handles and streams from the start write to the stop write.
#[derive(Clone)]
pub(super) struct AttPods {
    pub(super) greeting: Vec<Vec<u8>>,
    /// Whether they hang up the attribute channel, and only it, once the greeting is out.
    pub(super) hang_up: bool,
    /// The handles they list in answer to the walk; `None` answers no request at all.
    pub(super) table: Option<Vec<u16>>,
    pub(super) page: usize,
    /// How many requests of the walk they answer before they answer no more; `None` is every one.
    pub(super) answers: Option<u32>,
    pub(super) stopped_after: Duration,
    /// Whether they go on streaming after the stop write and never reply to it.
    pub(super) deaf_to_stop: bool,
    pub(super) declares: bool,
    /// The clock they stream on; `None` sends the opening thirteen at once and then a packet each turn of their loop.
    pub(super) pace: Option<Pace>,
    /// The frame counter they leave out of the stream, as a frame lost on the way.
    pub(super) withheld: Option<u16>,
    /// The number of stream packets after the opening thirteen at which they raise the interrupted flag.
    pub(super) interrupt_after: Option<u32>,
    /// The number of such packets at which they hang up the attribute channel.
    pub(super) hang_up_after: Option<u32>,
    /// The number of such packets at which they hang up the control channel.
    pub(super) hang_up_control_after: Option<u32>,
    /// The handles whose write requests and read requests they answer with an error response.
    pub(super) refused: Vec<u16>,
}

impl Default for AttPods {
    fn default() -> Self {
        Self {
            greeting: Vec::new(),
            hang_up: false,
            table: Some((1..=TABLE_END).collect()),
            page: PAGE,
            answers: None,
            stopped_after: Duration::ZERO,
            deaf_to_stop: false,
            declares: true,
            pace: None,
            withheld: None,
            interrupt_after: None,
            hang_up_after: None,
            hang_up_control_after: None,
            refused: Vec::new(),
        }
    }
}

/// The response to a write request or a read request, as `13` answers `12 19 00 01 00`, or the error response for a refused handle.
fn request_reply(part: &AttPods, pdu: &[u8]) -> Option<Vec<u8>> {
    let (opcode, refusal, reply) = match pdu {
        [att::WRITE_REQUEST, ..] => (
            att::WRITE_REQUEST,
            WRITE_NOT_PERMITTED,
            vec![WRITE_RESPONSE],
        ),
        [att::READ_REQUEST, ..] => (
            att::READ_REQUEST,
            READ_NOT_PERMITTED,
            [&[READ_RESPONSE][..], &READ_VALUE].concat(),
        ),
        _ => return None,
    };
    let handle = u16::from_le_bytes([*pdu.get(1)?, *pdu.get(2)?]);
    if part.refused.contains(&handle) {
        return Some(att::error_response(opcode, handle, refusal).to_vec());
    }
    Some(reply)
}

/// Up to `page` attributes of `table` from `start`, as `05 01 07 00 2a 2b` lists 0x0007, or the error that ends it.
fn listing(table: &[u16], start: u16, page: usize) -> Vec<u8> {
    let listed: Vec<u16> = table
        .iter()
        .copied()
        .filter(|handle| *handle >= start)
        .take(page)
        .collect();
    if listed.is_empty() {
        return att::error_response(FIND_INFORMATION, start, att::ATTRIBUTE_NOT_FOUND).to_vec();
    }
    let mut reply = vec![FIND_INFORMATION_RESPONSE, FORMAT_SHORT];
    for handle in listed {
        reply.extend(handle.to_le_bytes());
        reply.extend(MADE_UP_TYPE);
    }
    reply
}

/// Each of the four values `table` holds whatever `start` is, as `09 07 06 00 04 07 00 2a 2b` declares 0x0007 at 0x0006, or the error when it holds none.
fn declarations(table: &[u16], start: u16) -> Vec<u8> {
    let held: Vec<(u16, u8)> = DECLARED
        .into_iter()
        .filter(|(value, _)| table.contains(value))
        .collect();
    if held.is_empty() {
        return att::error_response(READ_BY_TYPE, start, att::ATTRIBUTE_NOT_FOUND).to_vec();
    }
    let mut reply = vec![READ_BY_TYPE_RESPONSE, DECLARATION_LEN];
    for (value, properties) in held {
        reply.extend(declared_at(value).to_le_bytes());
        reply.push(properties);
        reply.extend(value.to_le_bytes());
        reply.extend(MADE_UP_TYPE);
    }
    reply
}

#[derive(Default)]
pub(super) struct AttState {
    streaming: bool,
    started: Option<Instant>,
    opened: u8,
    packets: u8,
    frames: u16,
    answered: u32,
    stopped_due: Option<Instant>,
    /// The packets of the stream now running, or of the last one, that left after its opening thirteen.
    after_burst: u32,
}

impl AttState {
    pub(super) fn answer(&mut self, part: &AttPods, pdu: &[u8]) -> Vec<Vec<u8>> {
        if pdu == START {
            self.streaming = true;
            self.started = Some(Instant::now());
            self.opened = 0;
            self.frames = 0;
            self.after_burst = 0;
            let mut replies = vec![STARTED.to_vec()];
            while part.pace.is_none() && usize::from(self.opened) < BURST_PACKETS {
                replies.extend(self.packet(part.withheld));
            }
            return replies;
        }
        if pdu == STOP {
            if part.deaf_to_stop {
                return Vec::new();
            }
            self.streaming = false;
            if part.stopped_after.is_zero() {
                return vec![STOPPED.to_vec()];
            }
            self.stopped_due = Instant::now().checked_add(part.stopped_after);
            return Vec::new();
        }
        if let Some(reply) = request_reply(part, pdu) {
            return vec![reply];
        }
        let Some(table) = &part.table else {
            return Vec::new();
        };
        match pdu {
            [FIND_INFORMATION, ..] if part.answers.is_some_and(|most| self.answered >= most) => {
                Vec::new()
            }
            [FIND_INFORMATION, low, high, ..] => {
                self.answered = self.answered.saturating_add(1);
                let start = u16::from_le_bytes([*low, *high]);
                vec![listing(table, start, part.page)]
            }
            [READ_BY_TYPE, low, high, ..] if part.declares => {
                let start = u16::from_le_bytes([*low, *high]);
                vec![declarations(table, start)]
            }
            _ => Vec::new(),
        }
    }

    /// The next packet of the stream once it is due, and its number after the opening thirteen, which none of the thirteen has.
    pub(super) fn next_packet(&mut self, part: &AttPods) -> Option<(Vec<u8>, Option<u32>)> {
        if !self.streaming || part.pace.is_some_and(|pace| !self.due(pace)) {
            return None;
        }
        let opening = usize::from(self.opened) < BURST_PACKETS;
        let packet = self.packet(part.withheld)?;
        if opening {
            return Some((packet, None));
        }
        self.after_burst = self.after_burst.saturating_add(1);
        Some((packet, Some(self.after_burst)))
    }

    /// The frames of the next packet: those of the opening thirteen in turn, then one.
    fn next_frames(&self) -> u16 {
        let opening = BURST_FRAMES.get(usize::from(self.opened));
        opening.copied().unwrap_or(1)
    }

    /// Whether the last frame of the next packet has ended on the clock of `pace`, and an opening packet has reached its turn.
    fn due(&self, pace: Pace) -> bool {
        let Some(since_start) = self.started.map(|started| started.elapsed()) else {
            return false;
        };
        let frames = self.frames.saturating_add(self.next_frames());
        let ended = pace.frame.saturating_mul(u32::from(frames));
        let into_burst = BURST_SPAN.saturating_mul(u32::from(self.opened));
        let into_burst = into_burst.checked_div(BURST_GAPS).unwrap_or_default();
        let turn = BURST_AFTER.saturating_add(into_burst);
        let opening = usize::from(self.opened) < BURST_PACKETS;
        since_start.saturating_add(pace.lead) >= ended && !(opening && since_start < turn)
    }

    /// The next packet of the stream, or none when its one frame is withheld.
    fn packet(&mut self, withheld: Option<u16>) -> Option<Vec<u8>> {
        let frames = self.next_frames();
        self.opened = self.opened.saturating_add(1);
        self.audio(frames, withheld)
    }

    pub(super) fn late_reply(&mut self) -> Option<Vec<u8>> {
        let due = self.stopped_due?;
        if Instant::now() < due {
            return None;
        }
        self.stopped_due = None;
        Some(STOPPED.to_vec())
    }

    /// The stream ends, and a reply they held back is lost, when the loop has closed the channel.
    pub(super) fn cut(&mut self) {
        self.streaming = false;
        self.stopped_due = None;
    }

    /// One audio notification of the next `frames` frames but a withheld one, each the table of contents byte and bytes from the seeded generator.
    fn audio(&mut self, frames: u16, withheld: Option<u16>) -> Option<Vec<u8>> {
        let mut data = vec![TOC_CELT_WIDEBAND_20MS_MONO];
        data.extend(noise(FRAME_NOISE_LEN));
        let (flag, level) = FRAME_PREFIX;
        let first = self.frames;
        let coded: Vec<Frame<'_>> = (first..first.saturating_add(frames))
            .filter(|counter| withheld != Some(*counter))
            .map(|counter| Frame {
                flag,
                level,
                counter,
                data: &data,
            })
            .collect();
        self.frames = self.frames.saturating_add(frames);
        if coded.is_empty() {
            return None;
        }
        let packet = encode_audio(self.packets, &coded).unwrap();
        self.packets = self.packets.wrapping_add(1);
        Some(packet)
    }
}
