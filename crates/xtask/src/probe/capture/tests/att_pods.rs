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
/// The length of each declaration in a reply: its handle, the property byte, the value handle and a 16 bit type.
const DECLARATION_LEN: u8 = 0x07;
/// How many attributes they list in one reply unless a test says otherwise, so a walk of the table takes several requests.
const PAGE: usize = 3;
/// The last handle of the table they list unless a test says otherwise, past the four the stream uses.
pub(super) const TABLE_END: u16 = 0x0012;
/// How many packets a session opens with.
pub(super) const BURST_PACKETS: usize = 13;
/// The frames in each of the thirteen packets a session opens with, 63 in all, as a Mac was seen to get them.
const BURST_FRAMES: [u16; BURST_PACKETS] = [4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 4];
/// The codec bytes that follow the table of contents byte in every frame here.
const FRAME_NOISE_LEN: usize = 9;
/// The flag and the level every frame here carries; the loop logs them and reads neither.
const FRAME_PREFIX: (u8, u8) = (0x80, 0xd5);
/// Stands in for the 16 bit type of every attribute and declaration here; the walk reads handles and no types.
pub(super) const MADE_UP_TYPE: [u8; 2] = [0x2a, 0x2b];
/// The property byte of a declared value the host writes with no response.
const WRITES: u8 = att::PROPERTY_WRITE_WITHOUT_RESPONSE;
/// The property byte of a declared value the pods notify.
const NOTIFIES: u8 = att::PROPERTY_NOTIFY;
/// The four values the stream uses and the property each is declared with.
pub(super) const DECLARED: [(u16, u8); 4] = [
    (START_HANDLE, WRITES),
    (STOP_HANDLE, WRITES),
    (AUDIO_HANDLE, NOTIFIES),
    (STATUS_HANDLE, NOTIFIES),
];

/// The handle of the declaration of the value at `value`, the one just before it.
pub(super) fn declared_at(value: u16) -> u16 {
    value.saturating_sub(1)
}

/// Their part on the attribute channel; `AttPods::default()` lists a table that holds the four handles and streams from the start write to the stop write.
#[derive(Clone)]
pub(super) struct AttPods {
    /// Packets they send as soon as the channel is open.
    pub(super) greeting: Vec<Vec<u8>>,
    /// Whether they hang up the attribute channel, and only it, once the greeting is out.
    pub(super) hang_up: bool,
    /// The handles they list in answer to the walk; `None` answers no request at all.
    pub(super) table: Option<Vec<u16>>,
    /// How many attributes they list in one reply.
    pub(super) page: usize,
    /// How many requests of the walk they answer before they answer no more; `None` is every one.
    pub(super) answers: Option<u32>,
    /// How long after the stop write their reply to it follows.
    pub(super) stopped_after: Duration,
    /// Whether they go on streaming after the stop write and never reply to it.
    pub(super) deaf_to_stop: bool,
    /// Whether they answer the request for the declarations.
    pub(super) declares: bool,
    /// The number of stream packets after the opening thirteen at which they raise the interrupted flag.
    pub(super) interrupt_after: Option<u32>,
    /// The number of such packets at which they hang up the attribute channel.
    pub(super) hang_up_after: Option<u32>,
    /// The number of such packets at which they hang up the control channel.
    pub(super) hang_up_control_after: Option<u32>,
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
            interrupt_after: None,
            hang_up_after: None,
            hang_up_control_after: None,
        }
    }
}

/// The reply to a Find Information request from `start`: up to `page` attributes of `table`, as `05 01 07 00 2a 2b` lists handle 0x0007, or the error that ends it.
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

/// The reply to the one read of the declarations, which the probe asks from the first handle: an entry for each of the four values `table` holds whatever `start` is, as `09 07 06 00 04 07 00 2a 2b` declares at 0x0006 the value at 0x0007, or the error for `start` when it holds none.
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

/// What they have done on the attribute channel so far.
#[derive(Default)]
pub(super) struct AttState {
    streaming: bool,
    packets: u8,
    frames: u16,
    /// The requests of the walk they have answered.
    answered: u32,
    /// When a reply to the stop write that they hold back is due.
    stopped_due: Option<Instant>,
    /// The packets of the stream now running, or of the last one, that left after its opening thirteen.
    pub(super) after_burst: u32,
}

impl AttState {
    /// What they answer one packet of the loop with, in order.
    pub(super) fn answer(&mut self, part: &AttPods, pdu: &[u8]) -> Vec<Vec<u8>> {
        if pdu == START {
            self.streaming = true;
            self.frames = 0;
            self.after_burst = 0;
            let mut replies = vec![STARTED.to_vec()];
            replies.extend(BURST_FRAMES.map(|frames| self.audio(frames)));
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

    /// The next packet of the stream while it runs: one frame, which stands for 20 ms of audio.
    pub(super) fn next_packet(&mut self) -> Option<Vec<u8>> {
        if !self.streaming {
            return None;
        }
        Some(self.audio(1))
    }

    /// The reply to the stop write that they held back, once it is due.
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

    /// One audio notification of `frames` frames, each the table of contents byte and bytes from the seeded generator.
    fn audio(&mut self, frames: u16) -> Vec<u8> {
        let mut data = vec![TOC_CELT_WIDEBAND_20MS_MONO];
        data.extend(noise(FRAME_NOISE_LEN));
        let (flag, level) = FRAME_PREFIX;
        let first = self.frames;
        let coded: Vec<Frame<'_>> = (first..first.saturating_add(frames))
            .map(|counter| Frame {
                flag,
                level,
                counter,
                data: &data,
            })
            .collect();
        self.frames = self.frames.saturating_add(frames);
        let packet = encode_audio(self.packets, &coded).unwrap();
        self.packets = self.packets.wrapping_add(1);
        packet
    }
}
