//! Sorts a stretch of the attribute log into what each figure of the dictation stream reads.

use std::collections::BTreeMap;

use pods_proto::att::{self, AttPdu, CHARACTERISTIC_DECLARATION};
use pods_proto::dictation::{AudioNotification, DictationPacket, Status};
use pods_proto::sdulog::{Direction, Record};

use crate::probe::dictate::TypeText;

/// One whole frame of an audio notification, without its coded bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameSeen {
    pub(crate) counter: u16,
    pub(crate) flag: u8,
    pub(crate) level: u8,
    pub(crate) len: usize,
    /// The first coded byte, `None` for a frame with none.
    pub(crate) toc: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notification {
    /// Its place among the records that were loaded, counted from 0.
    pub(crate) record: usize,
    pub(crate) at_ns: u64,
    pub(crate) counter: u8,
    pub(crate) frames: Vec<FrameSeen>,
    pub(crate) length_matches: bool,
    /// Bytes after the last whole frame.
    pub(crate) trailing: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answers {
    Start,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reply {
    /// Its place among the records that were loaded, counted from 0.
    pub(crate) record: usize,
    pub(crate) at_ns: u64,
    pub(crate) answers: Answers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed {
    /// The type of the attribute, as `TypeText` writes it.
    pub(crate) uuid: String,
    /// The property byte of the declaration that names the handle as its value, `None` when none does.
    pub(crate) properties: Option<u8>,
}

#[derive(Debug, Default)]
pub(crate) struct LoadedAtt {
    pub(crate) audio: Vec<Notification>,
    pub(crate) replies: Vec<Reply>,
    pub(crate) handles: BTreeMap<u16, Listed>,
    pub(crate) requests: BTreeMap<u8, u32>,
    /// Packets from the pods no parser reads: one cut before its fixed fields, a list with no whole entry, a value on the audio handle without its marker.
    pub(crate) parse_failures: u32,
    pub(crate) from_pods: u32,
    /// Packets the host sent, which no figure reads.
    pub(crate) to_pods: u32,
    pub(crate) largest_pdu: Option<usize>,
}

impl LoadedAtt {
    pub(crate) fn frames(&self) -> impl Iterator<Item = &FrameSeen> {
        self.audio.iter().flat_map(|packet| &packet.frames)
    }

    fn take(&mut self, index: usize, record: &Record, declarations_asked: bool) {
        self.from_pods = self.from_pods.saturating_add(1);
        self.largest_pdu = self.largest_pdu.max(Some(record.sdu.len()));
        let reply = |answers| Reply {
            record: index,
            at_ns: record.at_ns,
            answers,
        };
        match DictationPacket::parse(&record.sdu) {
            Ok(Some(DictationPacket::Audio(audio))) => {
                self.audio.push(read_audio(index, record, &audio));
            }
            Ok(Some(DictationPacket::Status(Status::Started))) => {
                self.replies.push(reply(Answers::Start));
            }
            Ok(Some(DictationPacket::Status(Status::Stopped))) => {
                self.replies.push(reply(Answers::Stop));
            }
            Ok(Some(DictationPacket::Status(Status::Other(_)))) => {}
            Ok(Some(DictationPacket::Unknown { .. })) | Err(_) => {
                self.parse_failures = self.parse_failures.saturating_add(1);
            }
            Ok(None) => self.take_other(&record.sdu, declarations_asked),
        }
    }

    fn take_other(&mut self, pdu: &[u8], declarations_asked: bool) {
        match AttPdu::parse(pdu) {
            Ok(AttPdu::Information(pairs)) => {
                for pair in pairs {
                    let uuid = TypeText(pair.uuid).to_string();
                    let properties = None;
                    let listed = Listed { uuid, properties };
                    self.handles.entry(pair.handle).or_insert(listed);
                }
            }
            Ok(AttPdu::ByType(entries)) if declarations_asked => {
                for found in entries.filter_map(att::characteristic) {
                    let uuid = TypeText(found.uuid).to_string();
                    let properties = None;
                    let listed = Listed { uuid, properties };
                    let value = self.handles.entry(found.value_handle).or_insert(listed);
                    value.properties = Some(found.properties);
                }
            }
            Ok(AttPdu::Request { opcode }) => {
                let asked = self.requests.entry(opcode).or_insert(0);
                *asked = asked.saturating_add(1);
            }
            Ok(
                AttPdu::ByType(_)
                | AttPdu::Error { .. }
                | AttPdu::Notification { .. }
                | AttPdu::Indication { .. }
                | AttPdu::Other { .. },
            ) => {}
            Err(_) => self.parse_failures = self.parse_failures.saturating_add(1),
        }
    }
}

/// Nothing is dropped; a packet no figure reads is still counted.
pub(crate) fn load(records: &[Record]) -> LoadedAtt {
    let mut loaded = LoadedAtt::default();
    // A reply answers the last request of the host, so only a request changes what the next list is read as.
    let mut declarations_asked = false;
    for (index, record) in records.iter().enumerate() {
        match record.direction {
            Direction::ToPods => {
                loaded.to_pods = loaded.to_pods.saturating_add(1);
                if matches!(AttPdu::parse(&record.sdu), Ok(AttPdu::Request { .. })) {
                    declarations_asked = asks_declarations(&record.sdu);
                }
            }
            Direction::FromPods => loaded.take(index, record, declarations_asked),
        }
    }
    loaded
}

// Sample input: `08 01 00 ff ff 03 28`, a Read By Type request for the characteristic declarations of any range.
fn asks_declarations(pdu: &[u8]) -> bool {
    let [opcode, _, _, _, _, kind @ ..] =
        att::read_by_type_request(0, 0, CHARACTERISTIC_DECLARATION);
    matches!(pdu, [asked, _, _, _, _, of_kind @ ..] if *asked == opcode && *of_kind == kind)
}

fn read_audio(index: usize, record: &Record, audio: &AudioNotification<'_>) -> Notification {
    let mut walk = audio.frames();
    let frames = walk
        .by_ref()
        .map(|frame| FrameSeen {
            counter: frame.counter,
            flag: frame.flag,
            level: frame.level,
            len: frame.data.len(),
            toc: frame.data.first().copied(),
        })
        .collect();
    Notification {
        record: index,
        at_ns: record.at_ns,
        counter: audio.packet_counter,
        frames,
        length_matches: audio.length_matches(),
        // Read only after the walk above ended: before that it counts the frames not yet read too.
        trailing: walk.trailing(),
    }
}
