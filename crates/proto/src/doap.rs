//! The service the dictation stream runs on: its identifiers, its writes and the arm that finds it, with no I/O.

use crate::att::{self, AttPdu, ByType, Information, Uuid};
use crate::control::{ControlCommand, ControlId};

// The layouts are Apple's, read from its host software and seen between a Linux host and one pair of AirPods.

/// The stream that carries Siri and dictation audio.
pub const STREAM_ID: u16 = 1;
/// Opus, the only codec the pods offer on `STREAM_ID`.
pub const OPUS_CODEC: u8 = 7;
/// The reason of a stop the host asks for.
pub const STOP_REASON_HOST: u8 = 0x00;
/// The reason the pods refuse a start with, and a host gives for audio nobody asked for.
pub const STOP_REASON_UNSOLICITED: u8 = 0x06;
/// How many requests the walk and the pass of one arm may send; a chosen bound, since the walk reads the whole table and no other firmware's length is known.
pub const ARM_MAX_REQUESTS: u32 = 128;

/// The lowest attribute handle (Bluetooth Core Specification 6.1, Vol 3, Part F, 3.2.2).
const FIRST_HANDLE: u16 = 0x0001;
/// The highest attribute handle (Vol 3, Part F, 3.2.2).
const LAST_HANDLE: u16 = 0xffff;
/// The descriptor value that turns notifications on (Vol 3, Part G, 3.3.3.3).
const NOTIFY: [u8; 2] = [0x01, 0x00];
/// Characteristics of the service, in the order of `Handles`.
const CHARACTERISTICS: usize = 6;
/// The last four of them notify, so each has a descriptor to write.
const NOTIFYING: usize = 4;
/// Supported Codec, Select Codec, Start, Stop, Send Data and Event Indicator, each in wire order.
const UUIDS: [[u8; 16]; CHARACTERISTICS] = [
    0xf195_b8fb_a9e2_4401_858b_2f87_a069_28a8_u128.to_le_bytes(),
    0xe1f9_b835_7e47_413d_af94_c68e_574b_8f7e_u128.to_le_bytes(),
    0xa08c_e5ef_698a_42a2_b980_7f3a_c00b_3845_u128.to_le_bytes(),
    0x6288_ea2d_7b89_47ad_890b_9fa6_bf3c_fc58_u128.to_le_bytes(),
    0x3f1c_161d_6473_4746_91f5_6d27_6107_80c6_u128.to_le_bytes(),
    0xc7c6_947d_3165_4bcb_8eaf_b328_896c_b531_u128.to_le_bytes(),
];

/// Sample output: `04 00 04 00 09 00 13 01 00 00 00`, sent at the start of each dictation.
#[must_use]
pub const fn doap_mode_on() -> [u8; 11] {
    ControlCommand::switch(ControlId::DOAP_MODE, true).frame()
}

/// Sample output: `04 00 04 00 09 00 13 02 00 00 00`, which returns the stem to its usual action.
#[must_use]
pub const fn doap_mode_off() -> [u8; 11] {
    ControlCommand::switch(ControlId::DOAP_MODE, false).frame()
}

/// Where one table holds the service; a reconnect or another firmware may move every one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handles {
    pub supported_codecs: u16,
    pub select_codec: u16,
    pub start: u16,
    pub stop: u16,
    pub send_data: u16,
    pub event: u16,
    /// The descriptors of `start`, `stop`, `send_data` and `event`, in that order.
    pub cccds: [u16; NOTIFYING],
}

impl Handles {
    /// One firmware's table, for a tool and a test; an arm finds the real one.
    pub const OBSERVED: Self = Self {
        supported_codecs: 0x0003,
        select_codec: 0x0005,
        start: 0x0007,
        stop: 0x000a,
        send_data: 0x000d,
        event: 0x0010,
        cccds: [0x0008, 0x000b, 0x000e, 0x0011],
    };
}

/// Sample output for 0x0008: `12 08 00 01 00`, a Write Request the pods answer.
#[must_use]
pub const fn cccd_on(handle: u16) -> [u8; 5] {
    let [low, high] = handle.to_le_bytes();
    let [notify_low, notify_high] = NOTIFY;
    [att::WRITE_REQUEST, low, high, notify_low, notify_high]
}

/// Sample output for 0x0003: `0a 03 00`.
#[must_use]
pub const fn read(handle: u16) -> [u8; 3] {
    let [low, high] = handle.to_le_bytes();
    [att::READ_REQUEST, low, high]
}

/// Sample output: `52 05 00 01 01 00 07`, one stream, its identifier, its codec.
#[must_use]
pub const fn select_codec(handles: &Handles) -> [u8; 7] {
    let [low, high] = handles.select_codec.to_le_bytes();
    let [stream_low, stream_high] = STREAM_ID.to_le_bytes();
    let streams = 0x01;
    let [command, codec] = [att::WRITE_COMMAND, OPUS_CODEC];
    [command, low, high, streams, stream_low, stream_high, codec]
}

/// Sample output: `52 07 00 01 00`.
#[must_use]
pub const fn start(handles: &Handles) -> [u8; 5] {
    let [low, high] = handles.start.to_le_bytes();
    let [stream_low, stream_high] = STREAM_ID.to_le_bytes();
    [att::WRITE_COMMAND, low, high, stream_low, stream_high]
}

/// Sample output for `STOP_REASON_HOST`: `52 0a 00 01 00 00`.
#[must_use]
pub const fn stop(handles: &Handles, reason: u8) -> [u8; 6] {
    let [low, high] = handles.stop.to_le_bytes();
    let [stream_low, stream_high] = STREAM_ID.to_le_bytes();
    let command = att::WRITE_COMMAND;
    [command, low, high, stream_low, stream_high, reason]
}

/// Sample output for `01 00 04 01 07`: `52 10 00 01 00 04 01 07`, an event the host tells the pods.
#[must_use]
pub const fn event_write(handles: &Handles, value: &[u8; 5]) -> [u8; 8] {
    let [low, high] = handles.event.to_le_bytes();
    let [v0, v1, v2, v3, v4] = *value;
    [att::WRITE_COMMAND, low, high, v0, v1, v2, v3, v4]
}

/// What the host does with one packet of the pods while it arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmStep {
    /// A request the pods answer.
    Send(Vec<u8>),
    /// Nothing to send: the packet answered nothing and the request is still out, or the arm is over.
    Wait,
    /// Armed once `select` is written, which the pods do not answer.
    Done {
        select: [u8; 7],
        handles: Handles,
    },
    Failed(ArmFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmFailure {
    /// The table holds none of the six characteristics.
    NoService,
    MissingCharacteristic,
    /// A notifying characteristic has no descriptor before the next declaration.
    NoCccd,
    /// The pods do not offer `OPUS_CODEC` on `STREAM_ID`.
    NoCodec,
    Refused {
        request: u8,
        code: u8,
    },
    /// A reply that does not answer the request that is out, or that does not parse.
    Garbled,
    /// `ARM_MAX_REQUESTS` were answered and the table had not been read.
    TooLong,
}

/// Finds the service in the pods' table by its identifiers and subscribes to it, one reply at a time.
#[derive(Debug, Clone)]
pub struct Arming {
    /// How many requests of the walk and the pass have gone out.
    requests: u32,
    stage: Stage,
}

#[derive(Debug, Clone)]
enum Stage {
    Declarations(Declarations),
    Descriptors(Descriptors),
    /// The write of `handles.cccds[written]` is out.
    Subscribing {
        handles: Handles,
        written: usize,
    },
    /// The read of the codec list is out.
    Codecs(Handles),
    Over,
}

/// The walk of every characteristic declaration (Vol 3, Part G, 4.6.1).
#[derive(Debug, Clone)]
struct Declarations {
    /// The first handle of the request that is out.
    from: u16,
    values: [Option<u16>; CHARACTERISTICS],
    /// The last handle each characteristic may hold a descriptor at, the one before the next declaration.
    ends: [u16; CHARACTERISTICS],
    /// The characteristic the last declaration opened, whose end the next one sets.
    open: Option<usize>,
}

/// The one pass over the service's handles for its descriptors (Vol 3, Part G, 4.7.1).
#[derive(Debug, Clone)]
struct Descriptors {
    /// The first and the last handle of the request that is out.
    from: u16,
    last: u16,
    values: [u16; CHARACTERISTICS],
    /// Each notifying characteristic's value handle and the last handle its descriptor may stand at.
    ranges: [(u16, u16); NOTIFYING],
    cccds: [Option<u16>; NOTIFYING],
}

impl Arming {
    /// The request the host sends first: `08 01 00 ff ff 03 28`, every declaration of the table.
    #[must_use]
    pub fn new() -> (Self, Vec<u8>) {
        let walk = Declarations {
            from: FIRST_HANDLE,
            values: [None; CHARACTERISTICS],
            ends: [LAST_HANDLE; CHARACTERISTICS],
            open: None,
        };
        let arming = Self {
            requests: 1,
            stage: Stage::Declarations(walk),
        };
        (arming, declarations_request(FIRST_HANDLE).to_vec())
    }

    /// Takes one packet of the pods, as `13` answers a descriptor write; an arm that is over answers `Wait`.
    pub fn on_pdu(&mut self, pdu: &[u8]) -> ArmStep {
        let stage = std::mem::replace(&mut self.stage, Stage::Over);
        if matches!(stage, Stage::Over) {
            return ArmStep::Wait;
        }
        let step = match (stage, AttPdu::parse(pdu)) {
            // What the pods send unasked answers nothing, and whoever owns the channel answers it.
            (
                stage,
                Ok(
                    AttPdu::Notification { .. }
                    | AttPdu::Indication { .. }
                    | AttPdu::Request { .. }
                    | AttPdu::Other { .. },
                ),
            ) => {
                self.stage = stage;
                return ArmStep::Wait;
            }
            (Stage::Declarations(walk), Ok(AttPdu::ByType(entries))) => {
                self.on_declarations(walk, entries)
            }
            // No declaration from the asked handle on is how a table ends.
            (
                Stage::Declarations(walk),
                Ok(AttPdu::Error {
                    request: att::READ_BY_TYPE_REQUEST,
                    code: att::ATTRIBUTE_NOT_FOUND,
                    ..
                }),
            ) => self.start_descriptors(&walk),
            (Stage::Descriptors(pass), Ok(AttPdu::Information(pairs))) => {
                self.on_descriptors(pass, pairs)
            }
            (
                Stage::Descriptors(_),
                Ok(AttPdu::Error {
                    request: att::FIND_INFORMATION_REQUEST,
                    code: att::ATTRIBUTE_NOT_FOUND,
                    ..
                }),
            ) => Err(ArmFailure::NoCccd),
            (Stage::Subscribing { handles, written }, Ok(AttPdu::WriteResponse)) => {
                Ok(self.subscribe(handles, written.saturating_add(1)))
            }
            (Stage::Codecs(handles), Ok(AttPdu::ReadResponse { value })) => {
                match offers_opus(value) {
                    Some(true) => Ok(ArmStep::Done {
                        select: select_codec(&handles),
                        handles,
                    }),
                    Some(false) => Err(ArmFailure::NoCodec),
                    None => Err(ArmFailure::Garbled),
                }
            }
            (_, Ok(AttPdu::Error { request, code, .. })) => {
                Err(ArmFailure::Refused { request, code })
            }
            // A reply to another request, or one that does not parse, may not be the answer.
            (
                _,
                Ok(
                    AttPdu::Information(_)
                    | AttPdu::ByType(_)
                    | AttPdu::ReadResponse { .. }
                    | AttPdu::WriteResponse,
                )
                | Err(_),
            ) => Err(ArmFailure::Garbled),
        };
        step.unwrap_or_else(ArmStep::Failed)
    }

    /// Counts one more request of the walk or the pass, which `ARM_MAX_REQUESTS` bounds.
    fn ask(&mut self) -> Result<(), ArmFailure> {
        if self.requests >= ARM_MAX_REQUESTS {
            return Err(ArmFailure::TooLong);
        }
        self.requests = self.requests.saturating_add(1);
        Ok(())
    }

    fn on_declarations(
        &mut self,
        mut walk: Declarations,
        entries: ByType<'_>,
    ) -> Result<ArmStep, ArmFailure> {
        let mut last = None;
        for entry in entries {
            let declared = att::characteristic(entry).ok_or(ArmFailure::Garbled)?;
            walk.take(&declared);
            last = Some(declared.declaration);
        }
        // Each request starts past the one before it, so a reply that ends behind its request is one the walk cannot follow.
        let last = last.filter(|last| *last >= walk.from);
        // No handle follows 0xffff, so a listing that reaches it is the end of the table.
        let Some(next) = last.ok_or(ArmFailure::Garbled)?.checked_add(1) else {
            return self.start_descriptors(&walk);
        };
        self.ask()?;
        walk.from = next;
        self.stage = Stage::Declarations(walk);
        Ok(ArmStep::Send(declarations_request(next).to_vec()))
    }

    fn start_descriptors(&mut self, walk: &Declarations) -> Result<ArmStep, ArmFailure> {
        if walk.values.iter().all(Option::is_none) {
            return Err(ArmFailure::NoService);
        }
        let [Some(v0), Some(v1), Some(v2), Some(v3), Some(v4), Some(v5)] = walk.values else {
            return Err(ArmFailure::MissingCharacteristic);
        };
        let [_, _, e2, e3, e4, e5] = walk.ends;
        let last = e2.max(e3).max(e4).max(e5);
        // A descriptor stands behind its value, so the pass starts one past the lowest of them.
        let from = v2.min(v3).min(v4).min(v5).checked_add(1);
        let from = from
            .filter(|from| *from <= last)
            .ok_or(ArmFailure::NoCccd)?;
        self.ask()?;
        self.stage = Stage::Descriptors(Descriptors {
            from,
            last,
            values: [v0, v1, v2, v3, v4, v5],
            ranges: [(v2, e2), (v3, e3), (v4, e4), (v5, e5)],
            cccds: [None; NOTIFYING],
        });
        let request = att::find_information_request(from, last);
        Ok(ArmStep::Send(request.to_vec()))
    }

    fn on_descriptors(
        &mut self,
        mut pass: Descriptors,
        pairs: Information<'_>,
    ) -> Result<ArmStep, ArmFailure> {
        let mut last = None;
        for pair in pairs {
            if pair.uuid == Uuid::Short(att::CLIENT_CHARACTERISTIC_CONFIGURATION) {
                pass.take(pair.handle);
            }
            last = Some(pair.handle);
        }
        if let [Some(c0), Some(c1), Some(c2), Some(c3)] = pass.cccds {
            let [
                supported_codecs,
                select_codec,
                start,
                stop,
                send_data,
                event,
            ] = pass.values;
            let handles = Handles {
                supported_codecs,
                select_codec,
                start,
                stop,
                send_data,
                event,
                cccds: [c0, c1, c2, c3],
            };
            return Ok(self.subscribe(handles, 0));
        }
        let last = last.filter(|last| *last >= pass.from);
        let next = last.ok_or(ArmFailure::Garbled)?.checked_add(1);
        let next = next.filter(|next| *next <= pass.last);
        let next = next.ok_or(ArmFailure::NoCccd)?;
        self.ask()?;
        pass.from = next;
        let request = att::find_information_request(next, pass.last);
        self.stage = Stage::Descriptors(pass);
        Ok(ArmStep::Send(request.to_vec()))
    }

    /// Writes the descriptor at `written`, or reads the codec list once all four are written.
    fn subscribe(&mut self, handles: Handles, written: usize) -> ArmStep {
        if let Some(cccd) = handles.cccds.get(written) {
            self.stage = Stage::Subscribing { handles, written };
            return ArmStep::Send(cccd_on(*cccd).to_vec());
        }
        self.stage = Stage::Codecs(handles);
        ArmStep::Send(read(handles.supported_codecs).to_vec())
    }
}

impl Declarations {
    /// Keeps a declaration of the service, and ends the range of the one before it.
    fn take(&mut self, declared: &att::Characteristic<'_>) {
        let open = self.open.take();
        if let Some(end) = open.and_then(|open| self.ends.get_mut(open)) {
            *end = declared.declaration.saturating_sub(1);
        }
        let Uuid::Long(uuid) = declared.uuid else {
            return;
        };
        let Some(slot) = UUIDS.iter().position(|known| known == uuid) else {
            return;
        };
        // The first declaration of an identifier is the one kept.
        if let Some(value @ None) = self.values.get_mut(slot) {
            *value = Some(declared.value_handle);
            self.open = Some(slot);
        }
    }
}

impl Descriptors {
    /// Gives a descriptor to each notifying characteristic it stands behind, before the next declaration.
    fn take(&mut self, descriptor: u16) {
        for (cccd, (value, end)) in self.cccds.iter_mut().zip(self.ranges) {
            if cccd.is_none() && value < descriptor && descriptor <= end {
                *cccd = Some(descriptor);
            }
        }
    }
}

/// Sample output for 0x0005: `08 05 00 ff ff 03 28`, the declarations from that handle on.
fn declarations_request(from: u16) -> [u8; 7] {
    att::read_by_type_request(from, LAST_HANDLE, att::CHARACTERISTIC_DECLARATION)
}

// Sample input: `01 01 00 04 07 02 01 01`, one stream, id 1, a list of 4 bytes: codec 7 with 2 attribute bytes.
fn offers_opus(value: &[u8]) -> Option<bool> {
    let (streams, mut rest) = value.split_first()?;
    let mut offered = false;
    for _ in 0..*streams {
        let ([id_low, id_high, list_len], after) = rest.split_first_chunk::<3>()?;
        let (mut list, after_list) = after.split_at_checked(usize::from(*list_len))?;
        let ours = u16::from_le_bytes([*id_low, *id_high]) == STREAM_ID;
        while !list.is_empty() {
            let ([codec, attributes_len], after_codec) = list.split_first_chunk::<2>()?;
            list = after_codec.get(usize::from(*attributes_len)..)?;
            offered |= ours && *codec == OPUS_CODEC;
        }
        rest = after_list;
    }
    Some(offered)
}

#[cfg(test)]
mod tests;
