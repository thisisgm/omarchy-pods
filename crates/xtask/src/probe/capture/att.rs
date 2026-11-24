//! The attribute channel side of the loop: the walk of the pods' table, what the pods ask of the host, and the start and stop of the dictation stream.

use std::fmt;
use std::mem;

use pods_proto::att::{self, AttPdu};
use pods_proto::dictation::{self, DictationPacket, Event};
use pods_proto::doap::{ARM_MAX_REQUESTS, Handles};

use super::super::dictate::{self, HandleCheck, HandleWalk, TypeText, WalkState};
use super::super::script::Opening;
use super::guard::{Dictation, Session};
use super::record::{Bearer, IoFailure, Mark, Recorder};
use super::{EndReason, Engine, Flow, NS_PER_MS, record_dropped};

/// The handle of an error response that is about no attribute (Bluetooth Core Specification 6.1, Vol 3, Part F, 3.4.1.1).
const NO_HANDLE: u16 = 0x0000;

/// What the attribute channel is waiting for.
pub(super) enum Asked {
    Nothing,
    /// A Find Information request of the walk went out at `at_ns`.
    Walk {
        walk: HandleWalk,
        at_ns: u64,
    },
    /// The one Read By Type request of `att-map` went out at `at_ns`.
    Declarations {
        at_ns: u64,
    },
}

impl Engine<'_, '_> {
    pub(super) fn on_pdu(&mut self, at_ns: u64, pdu: &[u8]) -> Flow {
        self.recorder.received_pdu(at_ns, pdu)?;
        self.answer(at_ns, pdu)?;
        let stop_sent_ns = self.dropped.dictation_stop_ns.get();
        mark_status(self.recorder, at_ns, pdu, self.start_sent_ns, stop_sent_ns)?;
        match self.asked {
            Asked::Nothing => Ok(()),
            Asked::Walk { .. } => self.walk_on(pdu),
            Asked::Declarations { .. } => self.declarations_on(pdu),
        }
    }

    /// Refuses a request and confirms an indication, because the pods wait on both and one left unanswered may cost the channel (Vol 3, Part F, 3.3.3); a knock answers neither.
    fn answer(&mut self, at_ns: u64, pdu: &[u8]) -> Flow {
        let silent = self.script.opening == Opening::AttKnock;
        match AttPdu::parse(pdu) {
            Ok(AttPdu::Request { opcode }) if silent => {
                let text = format_args!("opcode {opcode:#04x} not answered: a knock sends nothing");
                Ok(self.recorder.mark(at_ns, Mark::AttRequest, &text)?)
            }
            Ok(AttPdu::Request { opcode }) => {
                let refusal = att::error_response(opcode, NO_HANDLE, att::REQUEST_NOT_SUPPORTED);
                let sent = self.wires.link(Bearer::Att)?.send(&refusal);
                if sent.result.is_ok() {
                    let refused = self.recorder.att_requests_refused;
                    self.recorder.att_requests_refused = refused.saturating_add(1);
                }
                let text = format_args!("opcode {opcode:#04x} refused");
                Ok(self
                    .recorder
                    .sent(sent, &refusal, Mark::AttRequest, &text)?)
            }
            Ok(AttPdu::Indication { .. }) if !silent => {
                let sent = self.wires.link(Bearer::Att)?.send(&att::CONFIRMATION);
                let confirmed =
                    self.recorder
                        .sent(sent, &att::CONFIRMATION, Mark::Sent, &"confirmation");
                Ok(confirmed?)
            }
            Ok(
                AttPdu::Indication { .. }
                | AttPdu::Information(_)
                | AttPdu::ByType(_)
                | AttPdu::Error { .. }
                | AttPdu::ReadResponse { .. }
                | AttPdu::WriteResponse
                | AttPdu::Notification { .. }
                | AttPdu::Other { .. },
            )
            | Err(_) => Ok(()),
        }
    }

    pub(super) fn start_walk(&mut self) -> Flow {
        self.ask_walk(HandleWalk::default(), HandleWalk::first())
    }

    /// As `04 0a 00 ff ff` asks from handle 0x000a, then waits for its answer.
    fn ask_walk(&mut self, walk: HandleWalk, request: [u8; 5]) -> Flow {
        let sent = self.wires.link(Bearer::Att)?.send(&request);
        let at_ns = sent.at_ns;
        self.asked = Asked::Walk { walk, at_ns };
        let [_opcode, low, high, ..] = request;
        let from = u16::from_le_bytes([low, high]);
        let text = format_args!("find information from {from:#06x}");
        Ok(self.recorder.sent(sent, &request, Mark::Sent, &text)?)
    }

    fn walk_on(&mut self, pdu: &[u8]) -> Flow {
        self.list(pdu)?;
        let asked = mem::replace(&mut self.asked, Asked::Nothing);
        let Asked::Walk { mut walk, at_ns } = asked else {
            self.asked = asked;
            return Ok(());
        };
        let next = walk.on_pdu(pdu);
        match (next, walk.state()) {
            (Some(request), _) => self.ask_walk(walk, request),
            (None, WalkState::Ended) => self.walk_over(&walk.result()),
            (None, WalkState::Cut) => {
                let why = format_args!("the table had not ended after {ARM_MAX_REQUESTS} requests");
                self.walk_cut(&walk.result(), &why)
            }
            (None, WalkState::Lost) => {
                let why = "the pods sent a reply the walk could not follow";
                self.walk_cut(&walk.result(), &why)
            }
            (None, WalkState::Asking) => {
                self.asked = Asked::Walk { walk, at_ns };
                Ok(())
            }
        }
    }

    /// Each as `attribute\t0x0007\t0x2803`.
    fn list(&mut self, pdu: &[u8]) -> Flow {
        let Ok(AttPdu::Information(pairs)) = AttPdu::parse(pdu) else {
            return Ok(());
        };
        for pair in pairs {
            let (handle, kind) = (pair.handle, TypeText(pair.uuid));
            let line = format_args!("attribute\t{handle:#06x}\t{kind}");
            self.recorder.handle_line(&line)?;
        }
        Ok(())
    }

    /// Marks what a walk found that the pods ended; a table that lacks a handle, or one the pods did not list and the owner did not waive, ends the run before any marker or write.
    fn walk_over(&mut self, check: &HandleCheck) -> Flow {
        let at_ns = self.wires.now();
        if self.script.opening == Opening::AttMap {
            self.recorder.mark(at_ns, Mark::Handles, &check)?;
            return self.ask_declarations();
        }
        match check {
            HandleCheck::Present => self.recorder.mark(at_ns, Mark::Handles, &check)?,
            HandleCheck::NoListing if self.script.unchecked => {
                self.recorder.mark(at_ns, Mark::Handles, &"unchecked")?;
            }
            HandleCheck::Missing(_) | HandleCheck::NoListing => {
                self.recorder.mark(at_ns, Mark::Handles, &check)?;
                return Err(EndReason::HandleCheck(check.clone()));
            }
        }
        self.checked = true;
        Ok(())
    }

    /// Closes the channel, as a request may still be out (Vol 3, Part F, 3.3.2); a waived empty walk goes on, on a fresh one.
    fn walk_cut(&mut self, check: &HandleCheck, why: &dyn fmt::Display) -> Flow {
        let at_ns = self.close_att(why)?;
        if self.script.opening == Opening::AttMap {
            return Ok(self.recorder.mark(at_ns, Mark::Handles, &check)?);
        }
        match check {
            HandleCheck::NoListing if self.script.unchecked => {
                self.connect_att()?;
                let at_ns = self.wires.now();
                self.recorder.mark(at_ns, Mark::Handles, &"unchecked")?;
                self.checked = true;
                Ok(())
            }
            HandleCheck::Present => {
                self.recorder.mark(at_ns, Mark::Handles, &check)?;
                Err(EndReason::WalkUnfinished)
            }
            HandleCheck::Missing(_) | HandleCheck::NoListing => {
                self.recorder.mark(at_ns, Mark::Handles, &check)?;
                Err(EndReason::HandleCheck(check.clone()))
            }
        }
    }

    /// Closes the attribute channel and marks why; the stamp is for the marks that follow.
    fn close_att(&mut self, why: &dyn fmt::Display) -> Result<u64, EndReason> {
        let at_ns = self.wires.now();
        drop(self.wires.att.take());
        self.recorder.mark(at_ns, Mark::AttClose, why)?;
        Ok(at_ns)
    }

    fn ask_declarations(&mut self) -> Flow {
        let request = dictate::declarations_request();
        let sent = self.wires.link(Bearer::Att)?.send(&request);
        let at_ns = sent.at_ns;
        self.asked = Asked::Declarations { at_ns };
        let text = "read by type characteristic declaration";
        Ok(self.recorder.sent(sent, &request, Mark::Sent, &text)?)
    }

    /// Each as `declaration\t0x0006\t0x04\t0x0007\t0x2b2a`; any reply ends the wait, an unasked packet does not.
    fn declarations_on(&mut self, pdu: &[u8]) -> Flow {
        let entries = match AttPdu::parse(pdu) {
            Ok(AttPdu::ByType(entries)) => entries,
            Ok(AttPdu::Error { .. } | AttPdu::Information(_)) | Err(_) => {
                self.asked = Asked::Nothing;
                return Ok(());
            }
            Ok(
                AttPdu::Notification { .. }
                | AttPdu::Indication { .. }
                | AttPdu::Request { .. }
                | AttPdu::ReadResponse { .. }
                | AttPdu::WriteResponse
                | AttPdu::Other { .. },
            ) => return Ok(()),
        };
        self.asked = Asked::Nothing;
        for entry in entries {
            let Some(found) = att::characteristic(entry) else {
                let len = entry.len();
                let line = format_args!("entry\t{len} bytes that are no declaration");
                self.recorder.handle_line(&line)?;
                continue;
            };
            let (at, properties) = (found.declaration, found.properties);
            let (value, kind) = (found.value_handle, TypeText(found.uuid));
            let line =
                format_args!("declaration\t{at:#06x}\t{properties:#04x}\t{value:#06x}\t{kind}");
            self.recorder.handle_line(&line)?;
        }
        Ok(())
    }

    pub(super) fn end_overdue_wait(&mut self, now_ns: u64) -> Flow {
        let asked_at_ns = match &self.asked {
            Asked::Nothing => return Ok(()),
            Asked::Walk { at_ns, .. } | Asked::Declarations { at_ns } => *at_ns,
        };
        let wait_ns = u64::from(self.timing.handle_walk_ms).saturating_mul(NS_PER_MS);
        if now_ns < asked_at_ns.saturating_add(wait_ns) {
            return Ok(());
        }
        let waited_ms = self.timing.handle_walk_ms;
        match mem::replace(&mut self.asked, Asked::Nothing) {
            Asked::Walk { walk, .. } => {
                let why = format_args!("a request of the walk got no answer in {waited_ms} ms");
                self.walk_cut(&walk.result(), &why)
            }
            Asked::Declarations { .. } => {
                let why =
                    format_args!("the read of the declarations got no answer in {waited_ms} ms");
                self.close_att(&why)?;
                Ok(())
            }
            Asked::Nothing => Ok(()),
        }
    }

    /// Whether a request is out, which holds the gate until its answer or its timeout.
    pub(super) fn waits_on_the_table(&self) -> bool {
        !matches!(self.asked, Asked::Nothing)
    }

    pub(super) fn mark_active(&mut self) -> Flow {
        // A second marker while a session is marked active was never seen, so it is not sent.
        if self.session.is_some() {
            return Ok(());
        }
        let link = self.wires.link(Bearer::Accessory)?;
        // The guard exists before the send, so a marker that failed half way still gets its idle marker.
        self.session = Some(Session {
            link,
            dropped: self.dropped,
        });
        let active = dictation::SESSION_ACTIVE;
        let sent = link.send(&active);
        let recorded = self
            .recorder
            .sent(sent, &active, Mark::Active, &"session active");
        Ok(recorded?)
    }

    pub(super) fn mark_idle(&mut self) -> Flow {
        // A script may mark idle a session this run did not mark active, as the rescue does.
        let (link, dropped) = (self.wires.link(Bearer::Accessory)?, self.dropped);
        drop(
            self.session
                .take()
                .unwrap_or_else(|| Session { link, dropped }),
        );
        Ok(record_dropped(self.recorder, self.dropped)?)
    }

    pub(super) fn dictation_start(&mut self) -> Flow {
        // A second start write is not known to be harmless, so a running stream is left as it is.
        if self.dictation.is_some() {
            return Ok(());
        }
        // The write goes only to a table the walk has checked, whatever the script says.
        if !self.checked {
            return Err(EndReason::HandleCheck(HandleCheck::NoListing));
        }
        let link = self.wires.link(Bearer::Att)?;
        // The guard exists before the send, so a start that failed half way still gets its stop.
        self.dictation = Some(Dictation {
            link,
            dropped: self.dropped,
        });
        let sent = link.send(&dictate::START);
        if sent.result.is_ok() {
            let starts = self.recorder.dictation_starts;
            self.recorder.dictation_starts = starts.saturating_add(1);
            self.start_sent_ns = Some(sent.at_ns);
        }
        let start = dictate::START;
        let recorded = self
            .recorder
            .sent(sent, &start, Mark::DictationStart, &"dictation start");
        Ok(recorded?)
    }

    pub(super) fn dictation_stop(&mut self) -> Flow {
        // A script may stop a stream this run did not start, as the rescue does.
        let (link, dropped) = (self.wires.link(Bearer::Att)?, self.dropped);
        drop(
            self.dictation
                .take()
                .unwrap_or_else(|| Dictation { link, dropped }),
        );
        Ok(record_dropped(self.recorder, self.dropped)?)
    }
}

/// Marks the two replies of the stream with how long each took, as `63000000 ns after the start write`.
pub(super) fn mark_status(
    recorder: &mut Recorder<'_>,
    at_ns: u64,
    pdu: &[u8],
    start_sent_ns: Option<u64>,
    stop_sent_ns: Option<u64>,
) -> Result<(), IoFailure> {
    let (kind, written_ns, write) = match DictationPacket::parse(pdu, &Handles::OBSERVED) {
        Ok(Some(DictationPacket::Event(Event::STARTED))) => (Mark::Started, start_sent_ns, "start"),
        Ok(Some(DictationPacket::Event(Event::StreamingEnd))) => {
            (Mark::Stopped, stop_sent_ns, "stop")
        }
        Ok(
            Some(
                DictationPacket::Event(
                    Event::Score { .. }
                    | Event::SiriActivation(_)
                    | Event::SiriState(_)
                    | Event::SiriCancel
                    | Event::Button(_)
                    | Event::Other { .. },
                )
                | DictationPacket::Audio(_)
                | DictationPacket::Stopped { .. }
                | DictationPacket::Unknown { .. },
            )
            | None,
        )
        | Err(_) => return Ok(()),
    };
    if let Some(written_ns) = written_ns {
        let delay_ns = at_ns.saturating_sub(written_ns);
        let text = format_args!("{delay_ns} ns after the {write} write");
        recorder.mark(at_ns, kind, &text)
    } else {
        let text = format_args!("no {write} write was sent by this run");
        recorder.mark(at_ns, kind, &text)
    }
}
