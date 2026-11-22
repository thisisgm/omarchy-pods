//! Synthetic btsnoop and `PacketLogger` traces for the tests, built byte by byte from the documented layouts; no byte comes from a real capture.

use pods_proto::dictation::{
    self, Frame, SESSION_ACTIVE, SESSION_IDLE, TOC_CELT_WIDEBAND_20MS_MONO,
};
use pods_proto::packets::{
    ATT_PSM, CONTROL_PSM, FEATURES_ACK_PREFIX, HANDSHAKE, HANDSHAKE_ACK_PREFIX,
};
use pods_proto::sdulog::Direction;
use pods_proto::uplink::{AccessUnit, OPAQUE_LEN, START, STOP, encode_audio};

/// A made up, locally administered address for the pods, in the order people read it.
pub(crate) const PODS_ADDRESS: [u8; 6] = [0x02, 0xa1, 0xb2, 0xc3, 0xd4, 0xe5];
/// A made up, locally administered address for the host's adapter.
pub(crate) const HOST_ADDRESS: [u8; 6] = [0x02, 0x5e, 0x6f, 0x70, 0x81, 0x92];
/// A made up device name, as a remote name event carries one.
pub(crate) const DEVICE_NAME: &str = "Synthetic Pods";
/// A made up link key, as a link key event carries one; no run of counting bytes, which the packets are full of.
pub(crate) const LINK_KEY: [u8; 16] = [
    0x9e, 0x37, 0x79, 0xb9, 0x7f, 0x4a, 0x7c, 0x15, 0xf3, 0x9c, 0xc0, 0x60, 0x5c, 0xed, 0xc8, 0x34,
];
/// What a keyboard on a second link sends, which no output may hold.
pub(crate) const KEYSTROKES: &[u8] = b"typed-on-a-keyboard";
/// A made up connection handle for the pods.
pub(crate) const PODS_HANDLE: u16 = 0x000b;
/// A made up connection handle for the keyboard.
pub(crate) const KEYBOARD_HANDLE: u16 = 0x000c;
/// A made up connection handle for the pods' voice link.
const VOICE_HANDLE: u16 = 0x0101;
/// 2026-11-20T12:00:00Z in microseconds since 1970, the moment every synthetic trace starts.
pub(crate) const START_UNIX_MICROS: u64 = 1_795_176_000_000_000;
/// Microseconds from the btsnoop epoch, midnight of year 0, to 1970: 62 168 256 000 seconds.
const YEAR_0_TO_1970_MICROS: u64 = 62_168_256_000_000_000;
/// How far the clock moves after every record unless a test sets it.
const STEP_MICROS: u64 = 1_000;
/// Microseconds in one millisecond.
const MICROS_PER_MILLI: u64 = 1_000;
/// The only version of the btsnoop format (`BlueZ` src/shared/btsnoop.c, `btsnoop_version`).
const BTSNOOP_VERSION: u32 = 1;
/// The datalink type of a trace `btmon -w` wrote.
pub(crate) const MONITOR_DATALINK: u32 = 2001;
/// Monitor opcode of a new adapter, whose record holds the adapter's address and name.
const OPCODE_NEW_INDEX: u16 = 0;
/// Monitor opcode of an HCI event.
const OPCODE_EVENT: u16 = 3;
/// Monitor opcode of ACL data the host sent.
const OPCODE_ACL_TX: u16 = 4;
/// Monitor opcode of ACL data the host received.
pub(crate) const OPCODE_ACL_RX: u16 = 5;
/// The packet boundary flag sits above the 12 bit handle (`BlueZ` lib/bluetooth/hci.h, `acl_flags`).
const BOUNDARY_SHIFT: u32 = 12;
/// Packet boundary flag of a continuing fragment.
pub(crate) const CONTINUATION: u16 = 0b01;
/// The two packet boundary flags a classic link starts a PDU with (Core 6.1 Vol 4 Part E 5.4.2).
pub(crate) const FIRST_NON_FLUSHABLE: u16 = 0b00;
pub(crate) const FIRST_FLUSHABLE: u16 = 0b10;
/// The L2CAP signalling channel of a classic link.
pub(crate) const SIGNALLING_CID: u16 = 0x0001;
/// L2CAP signalling codes.
pub(crate) const CONNECTION_REQUEST: u8 = 0x02;
pub(crate) const CONNECTION_RESPONSE: u8 = 0x03;
pub(crate) const CONFIGURATION_REQUEST: u8 = 0x04;
/// The identifier every synthetic signalling command carries; any value but zero is valid (Core 6.1 Vol 3 Part A 4).
const SIGNAL_IDENTIFIER: u8 = 0x01;
/// The result of a connection response that opens the channel (Core 6.1 Vol 3 Part A 4.3).
const RESULT_SUCCESS: u16 = 0x0000;
/// The type and the data length of the MTU option (Core 6.1 Vol 3 Part A 5.1).
const OPTION_MTU: [u8; 2] = [0x01, 0x02];
/// The type and the data length of the retransmission and flow control option (Core 6.1 Vol 3 Part A 5.4).
const OPTION_MODE: [u8; 2] = [0x04, 0x09];
/// HCI event codes.
pub(crate) const EVENT_CONNECTION_COMPLETE: u8 = 0x03;
pub(crate) const EVENT_DISCONNECTION_COMPLETE: u8 = 0x05;
pub(crate) const EVENT_REMOTE_NAME: u8 = 0x07;
pub(crate) const EVENT_LINK_KEY: u8 = 0x18;
pub(crate) const EVENT_SYNCHRONOUS_CONNECTION: u8 = 0x2c;
/// The status of an event that reports success.
const STATUS_SUCCESS: u8 = 0x00;
/// The type and the bus of a new adapter record: a primary controller on USB (`BlueZ` src/shared/btsnoop.h, `btsnoop_opcode_new_index`).
const PRIMARY_ON_USB: [u8; 2] = [0x00, 0x01];
/// What a connection complete event ends with: an ACL link, not encrypted (Core 6.1 Vol 4 Part E 7.7.3).
const ACL_NOT_ENCRYPTED: [u8; 2] = [0x01, 0x00];
/// The parameters of a remote name event: a status, an address and a 248 byte name (Core 6.1 Vol 4 Part E 7.7.7).
const REMOTE_NAME_PARAMETERS: usize = 255;
/// The key type a link key event ends with: an unauthenticated combination key (Core 6.1 Vol 4 Part E 7.7.24).
const KEY_TYPE: u8 = 0x04;
/// What a synchronous connection event ends with: eSCO, made up interval, window and packet lengths, transparent air mode (Core 6.1 Vol 4 Part E 7.7.35).
const VOICE_LINK: [u8; 8] = [0x02, 0x0c, 0x04, 0x3c, 0x00, 0x3c, 0x00, 0x03];
/// The reason a disconnection event ends with: the remote user ended the connection (Core 6.1 Vol 1 Part F 2.19).
const REMOTE_USER_ENDED: u8 = 0x13;
/// The PSM of AVDTP and the PSM of a keyboard's interrupt channel.
pub(crate) const AVDTP_PSM: u16 = 0x0019;
pub(crate) const HID_INTERRUPT_PSM: u16 = 0x0013;
/// AVDTP message types and signal identifiers.
pub(crate) const COMMAND: u8 = 0b00;
pub(crate) const ACCEPT: u8 = 0b10;
pub(crate) const GET_CAPABILITIES: u8 = 0x02;
pub(crate) const SET_CONFIGURATION: u8 = 0x03;
pub(crate) const AVDTP_START: u8 = 0x07;
pub(crate) const AVDTP_SUSPEND: u8 = 0x09;
pub(crate) const GET_ALL_CAPABILITIES: u8 = 0x0c;
/// AVDTP service categories.
pub(crate) const MEDIA_TRANSPORT: u8 = 0x01;
pub(crate) const MEDIA_CODEC: u8 = 0x07;
/// Stream end points 1 and 2; the number sits in the upper six bits of its byte (AVDTP 1.3 section 8.6.1).
pub(crate) const FIRST_SEID: u8 = 0x04;
const SECOND_SEID: u8 = 0x08;
/// Media type audio, the upper four bits of a Media Codec capability's first byte (AVDTP 1.3 section 8.21.5).
const MEDIA_TYPE_AUDIO: u8 = 0x00;
/// Media Codec Types: SBC, MPEG-2,4 AAC and a vendor's codec (A2DP 1.3 section 4.7).
pub(crate) const CODEC_SBC: u8 = 0x00;
pub(crate) const CODEC_AAC: u8 = 0x02;
pub(crate) const CODEC_VENDOR: u8 = 0xff;
/// The first two bytes of an RTP header: version 2, then the dynamic payload type 96 (RFC 3550 section 5.1).
const RTP_VERSION_AND_TYPE: [u8; 2] = [0x80, 0x60];
/// The timestamp and the synchronisation source of an RTP header, four bytes each, left zero (RFC 3550 section 5.1).
const RTP_STAMP_AND_SOURCE_LEN: usize = 8;
/// The channel identifiers the session trace uses, the host's then the pods'.
pub(crate) const CONTROL_CIDS: (u16, u16) = (0x0040, 0x0050);
pub(crate) const ATT_CIDS: (u16, u16) = (0x0041, 0x0051);
pub(crate) const SIGNALLING_CIDS: (u16, u16) = (0x0042, 0x0052);
pub(crate) const MEDIA_CIDS: (u16, u16) = (0x0043, 0x0053);
/// The MTU options the session trace sends for the control channel and the ATT channel.
pub(crate) const HOST_MTU: u16 = 672;
pub(crate) const PODS_MTU: u16 = 1024;
/// SBC capability bytes, made up.
pub(crate) const SBC_BYTES: [u8; 4] = [0xff, 0xff, 0x02, 0x35];
/// AAC capability bytes, made up.
pub(crate) const AAC_BYTES: [u8; 6] = [0x80, 0x01, 0x8c, 0x84, 0xe2, 0x00];
/// A vendor codec element: vendor 0x0000004c, codec 0x8001, then two made up bytes.
pub(crate) const VENDOR_BYTES: [u8; 8] = [0x4c, 0x00, 0x00, 0x00, 0x01, 0x80, 0xaa, 0xbb];
/// The bytes one synthetic media packet carries behind its RTP header.
pub(crate) const MEDIA_PAYLOAD_LEN: usize = 100;
/// The two control packets the session trace sends in fragments, by their length.
const THREE_FRAGMENT_LEN: usize = 600;
const TWO_FRAGMENT_LEN: usize = 300;
/// Where each of those is split: the size of every fragment but its last.
const THREE_FRAGMENT_SIZES: [usize; 2] = [200, 250];
const TWO_FRAGMENT_SIZES: [usize; 1] = [100];
/// What follows the prefix of the features acknowledgement in the macOS trace, made up.
const FEATURES_ACK_REST: [u8; 4] = [0x00; 4];
/// `PacketLogger` record types: a command, an event, ACL data sent and received, and synchronous data sent and received with the audio left out.
pub(crate) const PKLG_COMMAND: u8 = 0x00;
pub(crate) const PKLG_EVENT: u8 = 0x01;
pub(crate) const PKLG_ACL_SENT: u8 = 0x02;
pub(crate) const PKLG_ACL_RECEIVED: u8 = 0x03;
pub(crate) const PKLG_SCO_SENT: u8 = 0x18;
pub(crate) const PKLG_SCO_RECEIVED: u8 = 0x19;
/// What the length field of a `PacketLogger` record counts besides the payload: seconds, microseconds and the type.
pub(crate) const PKLG_COUNTED_LEN: u32 = 9;
/// Microseconds in one second.
const MICROS_PER_SECOND: u64 = 1_000_000;
/// The opcode of HCI Create Connection, whose parameters open with the address (Core 6.1 Vol 4 Part E 7.1.5).
const CREATE_CONNECTION: u16 = 0x0405;
/// What a Create Connection command ends with: made up packet types, page scan mode, clock offset and role switch.
const CONNECTION_PARAMETERS: [u8; 7] = [0x18, 0xcc, 0x01, 0x00, 0x00, 0x00, 0x01];
/// What a synchronous data record of the macOS trace holds where the audio would be, which no output may hold.
pub(crate) const VOICE_BYTES: &[u8] = b"voice-on-the-air";
/// An ATT notification and a write command for handles the dictation stream does not use, sent before the stream starts.
const OTHER_NOTIFICATION: [u8; 5] = [0x1b, 0x03, 0x00, 0x5a, 0xa5];
const OTHER_WRITE: [u8; 4] = [0x52, 0x03, 0x00, 0x01];
/// The flag and the level of a synthetic dictation frame.
const FRAME_FLAG: u8 = 0x80;
const FRAME_LEVEL: u8 = 200;

/// How a trace under construction lays its records out.
#[derive(Clone, Copy)]
enum Layout {
    Btsnoop,
    PacketLogger,
}

/// One trace under construction; records are stamped by a clock that steps after each.
pub(crate) struct Trace {
    bytes: Vec<u8>,
    micros: u64,
    handle: u16,
    layout: Layout,
}

impl Trace {
    /// An empty trace with the monitor datalink, on the pods' handle.
    pub(crate) fn new() -> Self {
        let mut bytes = b"btsnoop\0".to_vec();
        bytes.extend(BTSNOOP_VERSION.to_be_bytes());
        bytes.extend(MONITOR_DATALINK.to_be_bytes());
        Self {
            bytes,
            micros: START_UNIX_MICROS,
            handle: PODS_HANDLE,
            layout: Layout::Btsnoop,
        }
    }

    /// An empty `PacketLogger` trace, which has no file header, on the pods' handle.
    pub(crate) fn packet_logger() -> Self {
        Self {
            bytes: Vec::new(),
            micros: START_UNIX_MICROS,
            handle: PODS_HANDLE,
            layout: Layout::PacketLogger,
        }
    }

    /// Sets the clock to `ms` milliseconds after the start of the trace.
    pub(crate) fn at_ms(&mut self, ms: u64) {
        self.at_micros(ms.saturating_mul(MICROS_PER_MILLI));
    }

    /// Sets the clock to `micros` microseconds after the start of the trace.
    pub(crate) fn at_micros(&mut self, micros: u64) {
        self.micros = START_UNIX_MICROS.saturating_add(micros);
    }

    /// Sends what follows on another connection handle.
    pub(crate) fn on(&mut self, handle: u16) {
        self.handle = handle;
    }

    /// One `PacketLogger` record: the length of what follows it, seconds, microseconds, the type and the payload, the integers little endian.
    pub(crate) fn logged(&mut self, kind: u8, payload: &[u8]) {
        let len = u32::try_from(payload.len()).unwrap();
        let seconds = u32::try_from(self.micros / MICROS_PER_SECOND).unwrap();
        let micros = u32::try_from(self.micros % MICROS_PER_SECOND).unwrap();
        self.bytes
            .extend(len.saturating_add(PKLG_COUNTED_LEN).to_le_bytes());
        self.bytes.extend(seconds.to_le_bytes());
        self.bytes.extend(micros.to_le_bytes());
        self.bytes.push(kind);
        self.bytes.extend(payload);
        self.micros = self.micros.saturating_add(STEP_MICROS);
    }

    /// One monitor record on adapter 0, or the `PacketLogger` record of the same meaning.
    pub(crate) fn record(&mut self, opcode: u16, data: &[u8]) {
        if let Layout::PacketLogger = self.layout {
            let kind = match opcode {
                OPCODE_EVENT => PKLG_EVENT,
                OPCODE_ACL_TX => PKLG_ACL_SENT,
                OPCODE_ACL_RX => PKLG_ACL_RECEIVED,
                other => panic!("monitor opcode {other} has no PacketLogger type"),
            };
            return self.logged(kind, data);
        }
        let len = u32::try_from(data.len()).unwrap();
        self.bytes.extend(len.to_be_bytes());
        self.bytes.extend(len.to_be_bytes());
        self.bytes.extend(0_u16.to_be_bytes());
        self.bytes.extend(opcode.to_be_bytes());
        self.bytes.extend(0_u32.to_be_bytes());
        self.bytes.extend(
            YEAR_0_TO_1970_MICROS
                .saturating_add(self.micros)
                .to_be_bytes(),
        );
        self.bytes.extend(data);
        self.micros = self.micros.saturating_add(STEP_MICROS);
    }

    /// One HCI event: code, parameter length, parameters.
    pub(crate) fn event(&mut self, code: u8, parameters: &[u8]) {
        let mut packet = vec![code, u8::try_from(parameters.len()).unwrap()];
        packet.extend(parameters);
        self.record(OPCODE_EVENT, &packet);
    }

    /// A Disconnection Complete event for `handle`, after which the link is gone.
    pub(crate) fn end_link(&mut self, handle: u16) {
        let mut ended = vec![STATUS_SUCCESS];
        ended.extend(handle.to_le_bytes());
        ended.push(REMOTE_USER_ENDED);
        self.event(EVENT_DISCONNECTION_COMPLETE, &ended);
    }

    /// One ACL data packet with the given packet boundary flag.
    pub(crate) fn acl(&mut self, direction: Direction, boundary: u16, data: &[u8]) {
        let mut packet = (self.handle | (boundary << BOUNDARY_SHIFT))
            .to_le_bytes()
            .to_vec();
        packet.extend(u16::try_from(data.len()).unwrap().to_le_bytes());
        packet.extend(data);
        let opcode = match direction {
            Direction::ToPods => OPCODE_ACL_TX,
            Direction::FromPods => OPCODE_ACL_RX,
        };
        self.record(opcode, &packet);
    }

    /// One L2CAP PDU cut into ACL packets of the given sizes, the last one taking what is left.
    pub(crate) fn fragments(
        &mut self,
        direction: Direction,
        cid: u16,
        payload: &[u8],
        sizes: &[usize],
    ) {
        let mut pdu = u16::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
        pdu.extend(cid.to_le_bytes());
        pdu.extend(payload);
        let mut rest = pdu.as_slice();
        let mut boundary = start_flag(direction);
        for size in sizes {
            let (piece, after) = rest.split_at(*size);
            self.acl(direction, boundary, piece);
            rest = after;
            boundary = CONTINUATION;
        }
        self.acl(direction, boundary, rest);
    }

    /// One L2CAP PDU in one ACL packet.
    pub(crate) fn pdu(&mut self, direction: Direction, cid: u16, payload: &[u8]) {
        self.fragments(direction, cid, payload, &[]);
    }

    /// One signalling command in a PDU of its own.
    pub(crate) fn signal(&mut self, direction: Direction, code: u8, data: &[u8]) {
        let mut command = vec![code, SIGNAL_IDENTIFIER];
        command.extend(u16::try_from(data.len()).unwrap().to_le_bytes());
        command.extend(data);
        self.pdu(direction, SIGNALLING_CID, &command);
    }

    /// A connection request for `psm` and the response that answers it with `result`.
    pub(crate) fn connect(
        &mut self,
        requester: Direction,
        psm: u16,
        cids: (u16, u16),
        result: u16,
    ) {
        let (requester_cid, responder_cid) = match requester {
            Direction::ToPods => cids,
            Direction::FromPods => (cids.1, cids.0),
        };
        let mut request = psm.to_le_bytes().to_vec();
        request.extend(requester_cid.to_le_bytes());
        self.signal(requester, CONNECTION_REQUEST, &request);
        let mut response = responder_cid.to_le_bytes().to_vec();
        response.extend(requester_cid.to_le_bytes());
        response.extend(result.to_le_bytes());
        response.extend(0_u16.to_le_bytes());
        self.signal(other(requester), CONNECTION_RESPONSE, &response);
    }

    /// Opens a channel; `cids` are the host's identifier, then the pods'.
    pub(crate) fn open(&mut self, requester: Direction, psm: u16, cids: (u16, u16)) {
        self.connect(requester, psm, cids, RESULT_SUCCESS);
    }

    /// A configuration request from `sender` for the channel with `cids`, carrying one MTU option.
    pub(crate) fn offer_mtu(&mut self, sender: Direction, cids: (u16, u16), mtu: u16) {
        let mut request = receiver_cid(sender, cids).to_le_bytes().to_vec();
        request.extend(0_u16.to_le_bytes());
        request.extend(OPTION_MTU);
        request.extend(mtu.to_le_bytes());
        self.signal(sender, CONFIGURATION_REQUEST, &request);
    }

    /// A configuration request from `sender` for the channel with `cids`, asking for an L2CAP mode.
    pub(crate) fn offer_mode(&mut self, sender: Direction, cids: (u16, u16), mode: u8) {
        let mut request = receiver_cid(sender, cids).to_le_bytes().to_vec();
        request.extend(0_u16.to_le_bytes());
        request.extend(mode_option(mode));
        self.signal(sender, CONFIGURATION_REQUEST, &request);
    }

    /// One PDU from `sender` on the channel with `cids`.
    pub(crate) fn send(&mut self, sender: Direction, cids: (u16, u16), payload: &[u8]) {
        self.pdu(sender, receiver_cid(sender, cids), payload);
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// A retransmission and flow control option: the mode first, then made up windows and timeouts.
pub(crate) fn mode_option(mode: u8) -> Vec<u8> {
    let mut option = OPTION_MODE.to_vec();
    option.push(mode);
    option.extend([0x3f, 0x03, 0xd0, 0x07, 0xe0, 0x2e, 0xf0, 0x03]);
    option
}

/// The flag these traces start a PDU with, a different one for each side; a reader must take either from both.
pub(crate) fn start_flag(direction: Direction) -> u16 {
    match direction {
        Direction::ToPods => FIRST_NON_FLUSHABLE,
        Direction::FromPods => FIRST_FLUSHABLE,
    }
}

pub(crate) fn other(direction: Direction) -> Direction {
    match direction {
        Direction::ToPods => Direction::FromPods,
        Direction::FromPods => Direction::ToPods,
    }
}

/// The identifier a PDU from `sender` carries: the receiving side's own.
pub(crate) fn receiver_cid(sender: Direction, cids: (u16, u16)) -> u16 {
    match sender {
        Direction::ToPods => cids.1,
        Direction::FromPods => cids.0,
    }
}

/// One AVDTP signal in a single packet.
pub(crate) fn avdtp(label: u8, message: u8, signal: u8, body: &[u8]) -> Vec<u8> {
    let mut packet = vec![(label << 4) | message, signal];
    packet.extend(body);
    packet
}

/// One AVDTP service capability: category, length, data.
pub(crate) fn capability(category: u8, data: &[u8]) -> Vec<u8> {
    let mut capability = vec![category, u8::try_from(data.len()).unwrap()];
    capability.extend(data);
    capability
}

/// A Media Codec capability for an audio codec.
pub(crate) fn media_codec(codec_type: u8, bytes: &[u8]) -> Vec<u8> {
    let mut data = vec![MEDIA_TYPE_AUDIO, codec_type];
    data.extend(bytes);
    capability(MEDIA_CODEC, &data)
}

/// One media packet: an RTP version 2 header with this sequence number, then `MEDIA_PAYLOAD_LEN` bytes.
pub(crate) fn rtp(sequence: u16) -> Vec<u8> {
    let mut packet = RTP_VERSION_AND_TYPE.to_vec();
    packet.extend(sequence.to_be_bytes());
    packet.extend([0; RTP_STAMP_AND_SOURCE_LEN]);
    packet.extend(counting_bytes(MEDIA_PAYLOAD_LEN));
    packet
}

// Sample output for 3: 00 01 02, counting bytes that stand in for one packet.
pub(crate) fn counting_bytes(len: usize) -> Vec<u8> {
    std::iter::successors(Some(0_u8), |byte| Some(byte.wrapping_add(1)))
        .take(len)
        .collect()
}

/// The control channel packets of the session trace, in order.
pub(crate) fn control_packets() -> Vec<(Direction, Vec<u8>)> {
    let mut acknowledgement = HANDSHAKE_ACK_PREFIX.to_vec();
    acknowledgement.extend([0x00; 8]);
    let silent = AccessUnit {
        stamp: 480,
        data: &[0x00, 0x00],
    };
    let audio = encode_audio(&[0xa0; OPAQUE_LEN], &[silent; 4]).unwrap();
    vec![
        (Direction::ToPods, HANDSHAKE.to_vec()),
        (Direction::FromPods, acknowledgement),
        (Direction::ToPods, START.to_vec()),
        (Direction::FromPods, audio),
        (Direction::FromPods, counting_bytes(THREE_FRAGMENT_LEN)),
        (Direction::ToPods, counting_bytes(TWO_FRAGMENT_LEN)),
        (Direction::ToPods, STOP.to_vec()),
    ]
}

/// The attribute channel packets of the session trace: a notification from the pods and a write command to them.
pub(crate) fn att_packets() -> Vec<(Direction, Vec<u8>)> {
    vec![
        (
            Direction::FromPods,
            vec![
                0x1b, 0x0d, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0xb8, 0x01, 0x02,
            ],
        ),
        (Direction::ToPods, vec![0x52, 0x07, 0x00, 0x01, 0x00]),
    ]
}

/// An address in the order a packet carries it.
fn wire(address: [u8; 6]) -> Vec<u8> {
    address.into_iter().rev().collect()
}

/// The identifying events a real trace opens with: the adapter, the connection, the name and the link key.
fn identify(trace: &mut Trace) {
    let mut adapter = PRIMARY_ON_USB.to_vec();
    adapter.extend(wire(HOST_ADDRESS));
    adapter.extend(b"hci0\0\0\0\0");
    trace.record(OPCODE_NEW_INDEX, &adapter);
    paired(trace);
}

/// The events that name the pods: the connection, the name and the link key.
fn paired(trace: &mut Trace) {
    let mut connected = vec![STATUS_SUCCESS];
    connected.extend(PODS_HANDLE.to_le_bytes());
    connected.extend(wire(PODS_ADDRESS));
    connected.extend(ACL_NOT_ENCRYPTED);
    trace.event(EVENT_CONNECTION_COMPLETE, &connected);
    let mut named = vec![STATUS_SUCCESS];
    named.extend(wire(PODS_ADDRESS));
    named.extend(DEVICE_NAME.as_bytes());
    named.resize(REMOTE_NAME_PARAMETERS, 0);
    trace.event(EVENT_REMOTE_NAME, &named);
    let mut keyed = wire(PODS_ADDRESS);
    keyed.extend(LINK_KEY);
    keyed.push(KEY_TYPE);
    trace.event(EVENT_LINK_KEY, &keyed);
}

/// A keyboard on a second link opens a channel whose identifiers equal the control channel's, and types.
fn keyboard(trace: &mut Trace) {
    trace.on(KEYBOARD_HANDLE);
    trace.open(Direction::FromPods, HID_INTERRUPT_PSM, CONTROL_CIDS);
    trace.on(PODS_HANDLE);
    keystrokes(trace);
}

/// The keyboard types; only the connection handle tells its packet from one on the pods' control channel.
fn keystrokes(trace: &mut Trace) {
    trace.on(KEYBOARD_HANDLE);
    trace.send(Direction::FromPods, CONTROL_CIDS, KEYSTROKES);
    trace.on(PODS_HANDLE);
}

/// A voice link opens; its event is one the reader keeps, and it carries the pods' address.
fn voice(trace: &mut Trace) {
    let mut opened = vec![STATUS_SUCCESS];
    opened.extend(VOICE_HANDLE.to_le_bytes());
    opened.extend(PODS_ADDRESS.into_iter().rev());
    opened.extend(VOICE_LINK);
    trace.event(EVENT_SYNCHRONOUS_CONNECTION, &opened);
}

/// The control channel and the attribute channel, with one packet in three fragments and one in two.
fn accessory(trace: &mut Trace) {
    trace.open(Direction::ToPods, CONTROL_PSM, CONTROL_CIDS);
    trace.offer_mtu(Direction::ToPods, CONTROL_CIDS, HOST_MTU);
    trace.offer_mtu(Direction::FromPods, CONTROL_CIDS, PODS_MTU);
    for (direction, packet) in control_packets() {
        let sizes: &[usize] = match packet.len() {
            THREE_FRAGMENT_LEN => &THREE_FRAGMENT_SIZES,
            TWO_FRAGMENT_LEN => &TWO_FRAGMENT_SIZES,
            _ => &[],
        };
        let cid = receiver_cid(direction, CONTROL_CIDS);
        trace.fragments(direction, cid, &packet, sizes);
    }
    trace.open(Direction::FromPods, ATT_PSM, ATT_CIDS);
    trace.offer_mtu(Direction::ToPods, ATT_CIDS, HOST_MTU);
    trace.offer_mtu(Direction::FromPods, ATT_CIDS, PODS_MTU);
    for (direction, packet) in att_packets() {
        trace.send(direction, ATT_CIDS, &packet);
    }
}

/// The AVDTP exchange and the media: six packets with one sequence gap across the wrap, a suspend, a start, two more.
fn music(trace: &mut Trace) {
    let host = Direction::ToPods;
    let pods = Direction::FromPods;
    trace.at_ms(1_000);
    trace.open(host, AVDTP_PSM, SIGNALLING_CIDS);
    let mut offered = capability(MEDIA_TRANSPORT, &[]);
    offered.extend(media_codec(CODEC_SBC, &SBC_BYTES));
    let mut also_offered = media_codec(CODEC_AAC, &AAC_BYTES);
    also_offered.extend(media_codec(CODEC_VENDOR, &VENDOR_BYTES));
    let mut chosen = vec![FIRST_SEID, FIRST_SEID];
    chosen.extend(media_codec(CODEC_AAC, &AAC_BYTES));
    for (sender, packet) in [
        (host, avdtp(1, COMMAND, GET_ALL_CAPABILITIES, &[FIRST_SEID])),
        (pods, avdtp(1, ACCEPT, GET_ALL_CAPABILITIES, &offered)),
        (host, avdtp(2, COMMAND, GET_CAPABILITIES, &[SECOND_SEID])),
        (pods, avdtp(2, ACCEPT, GET_CAPABILITIES, &also_offered)),
        (host, avdtp(3, COMMAND, SET_CONFIGURATION, &chosen)),
        (pods, avdtp(3, ACCEPT, SET_CONFIGURATION, &[])),
    ] {
        trace.send(sender, SIGNALLING_CIDS, &packet);
    }
    trace.open(host, AVDTP_PSM, MEDIA_CIDS);
    trace.at_ms(1_990);
    trace.send(
        host,
        SIGNALLING_CIDS,
        &avdtp(4, COMMAND, AVDTP_START, &[FIRST_SEID]),
    );
    for (ms, sequence) in [
        (2_000, 65_533),
        (2_020, 65_534),
        (2_040, 65_535),
        (2_060, 0),
        (2_080, 1),
        (2_100, 4),
    ] {
        trace.at_ms(ms);
        trace.send(host, MEDIA_CIDS, &rtp(sequence));
    }
    trace.at_ms(2_500);
    trace.send(
        host,
        SIGNALLING_CIDS,
        &avdtp(5, COMMAND, AVDTP_SUSPEND, &[FIRST_SEID]),
    );
    trace.at_ms(3_100);
    trace.send(
        host,
        SIGNALLING_CIDS,
        &avdtp(6, COMMAND, AVDTP_START, &[FIRST_SEID]),
    );
    for (ms, sequence) in [(3_200, 5), (3_220, 6)] {
        trace.at_ms(ms);
        trace.send(host, MEDIA_CIDS, &rtp(sequence));
    }
}

/// One whole synthetic session: identifying events, a keyboard, the accessory channels, more typing, a voice link, music, then the link ends.
pub(crate) fn session() -> Trace {
    let mut trace = Trace::new();
    identify(&mut trace);
    keyboard(&mut trace);
    accessory(&mut trace);
    keystrokes(&mut trace);
    voice(&mut trace);
    music(&mut trace);
    trace.end_link(PODS_HANDLE);
    trace
}

/// The accessory packets of the macOS trace, which holds no handshake: macOS never sends one.
pub(crate) fn mac_accessory_packets() -> Vec<(Direction, Vec<u8>)> {
    let mut acknowledgement = FEATURES_ACK_PREFIX.to_vec();
    acknowledgement.extend(FEATURES_ACK_REST);
    vec![
        (Direction::ToPods, SESSION_IDLE.to_vec()),
        (Direction::FromPods, acknowledgement),
        (Direction::ToPods, SESSION_ACTIVE.to_vec()),
        (Direction::FromPods, counting_bytes(THREE_FRAGMENT_LEN)),
        (Direction::ToPods, SESSION_IDLE.to_vec()),
    ]
}

/// One audio notification that holds one frame, the table of contents byte alone.
pub(crate) fn audio_notification(counter: u8) -> Vec<u8> {
    let frame = Frame {
        flag: FRAME_FLAG,
        level: FRAME_LEVEL,
        counter: u16::from(counter),
        data: &[TOC_CELT_WIDEBAND_20MS_MONO],
    };
    dictation::encode_audio(counter, &[frame]).unwrap()
}

/// The attribute packets of the macOS trace: one dictation session, after a packet each way that is not part of it.
pub(crate) fn mac_att_packets() -> Vec<(Direction, Vec<u8>)> {
    vec![
        (Direction::FromPods, OTHER_NOTIFICATION.to_vec()),
        (Direction::ToPods, OTHER_WRITE.to_vec()),
        (Direction::ToPods, dictation::START.to_vec()),
        (Direction::FromPods, dictation::STARTED.to_vec()),
        (Direction::FromPods, audio_notification(0)),
        (Direction::FromPods, audio_notification(1)),
        (Direction::ToPods, dictation::STOP.to_vec()),
        (Direction::FromPods, dictation::STOPPED.to_vec()),
    ]
}

/// How a trace macOS wrote opens when recording starts after the accessory exchange: the events that name the pods and a keyboard that types.
pub(crate) fn mac_naming() -> Trace {
    let mut trace = Trace::packet_logger();
    let mut connect = CREATE_CONNECTION.to_le_bytes().to_vec();
    connect.push(
        u8::try_from(
            PODS_ADDRESS
                .len()
                .saturating_add(CONNECTION_PARAMETERS.len()),
        )
        .unwrap(),
    );
    connect.extend(wire(PODS_ADDRESS));
    connect.extend(CONNECTION_PARAMETERS);
    trace.logged(PKLG_COMMAND, &connect);
    paired(&mut trace);
    keystrokes(&mut trace);
    trace
}

/// How a trace macOS wrote opens: the naming events, then the accessory packets; no channel is opened by a request.
pub(crate) fn mac_opening() -> Trace {
    let mut trace = mac_naming();
    for (direction, packet) in mac_accessory_packets() {
        let sizes: &[usize] = match packet.len() {
            THREE_FRAGMENT_LEN => &THREE_FRAGMENT_SIZES,
            _ => &[],
        };
        let cid = receiver_cid(direction, CONTROL_CIDS);
        trace.fragments(direction, cid, &packet, sizes);
    }
    trace
}

/// One whole synthetic session as macOS logs it: one dictation session, and synchronous data records that hold no audio.
pub(crate) fn mac_session() -> Trace {
    let mut trace = mac_opening();
    for (direction, packet) in mac_att_packets() {
        trace.send(direction, ATT_CIDS, &packet);
    }
    voice(&mut trace);
    for kind in [PKLG_SCO_SENT, PKLG_SCO_RECEIVED, PKLG_SCO_RECEIVED] {
        trace.logged(kind, VOICE_BYTES);
    }
    let start = avdtp(1, COMMAND, AVDTP_START, &[FIRST_SEID]);
    trace.send(Direction::ToPods, SIGNALLING_CIDS, &start);
    for sequence in 0..2 {
        trace.send(Direction::ToPods, MEDIA_CIDS, &rtp(sequence));
    }
    trace.end_link(PODS_HANDLE);
    trace
}
