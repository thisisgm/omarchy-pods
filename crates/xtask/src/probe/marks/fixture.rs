//! Synthetic traces for the connection marks tests.

use pods_proto::packets::CONTROL_PSM;
use pods_proto::sdulog::Direction;

use super::hci_fixture::{
    ACCEPT, AVDTP_PSM, AVDTP_START, COMMAND, FIRST_SEID, MEDIA_CIDS, PODS_ADDRESS, PODS_HANDLE,
    RESULT_SUCCESS, SET_CONFIGURATION, SIGNALLING_CIDS, Trace, avdtp, rtp, wire,
};

/// A made up, locally administered address for a second set of pods, which no mark of the first may come from.
pub(crate) const OTHER_PODS_ADDRESS: [u8; 6] = [0x02, 0x17, 0x28, 0x39, 0x4a, 0x5b];
pub(crate) const OTHER_PODS_HANDLE: u16 = 0x000d;
const EVENT_CONNECTION_REQUEST: u8 = 0x04;
const EVENT_LE_META: u8 = 0x3e;
/// The two LE Meta subevents that report an advertisement (Core 6.1 Vol 4 Part E 7.7.65.2 and 7.7.65.13).
const LE_ADVERTISING_REPORT: u8 = 0x02;
const LE_EXTENDED_ADVERTISING_REPORT: u8 = 0x0d;
/// The event types of connectable reports and the address type of a random address.
const ONE_LEGACY_REPORT: [u8; 3] = [0x01, 0x00, 0x01];
const ONE_EXTENDED_REPORT: [u8; 4] = [0x01, 0x13, 0x00, 0x01];
/// What follows the address of an extended report: both PHYs, no set, no power, a made up RSSI, no period, no directed address.
const EXTENDED_REPORT_REST: [u8; 14] = [
    0x01, 0x00, 0xff, 0x7f, 0xc4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
/// Synthetic advertising data: flags followed by Apple manufacturer data with proximity pairing or a different type.
pub(crate) const PROXIMITY_ADVERT: [u8; 10] =
    [0x02, 0x01, 0x1a, 0x06, 0xff, 0x4c, 0x00, 0x07, 0x01, 0x00];
pub(crate) const OTHER_APPLE_ADVERT: [u8; 7] = [0x06, 0xff, 0x4c, 0x00, 0x10, 0x01, 0x00];
pub(crate) const OTHER_COMPANY_ADVERT: [u8; 7] = [0x06, 0xff, 0x00, 0x00, 0x07, 0x01, 0x00];
/// A synthetic random address, distinct from either device's ACL address.
pub(crate) const RANDOM_ADVERT_ADDRESS: [u8; 6] = [0x42, 0x00, 0x00, 0x00, 0x00, 0x01];
/// The RSSI a legacy report ends with, made up.
const REPORT_RSSI: u8 = 0xc4;
/// What a connection request ends with: a made up class of device, then an ACL link (Core 6.1 Vol 4 Part E 7.7.4).
const AUDIO_CLASS_ON_ACL: [u8; 4] = [0x18, 0x04, 0x24, 0x01];
/// The status of a connection that never came up: the page timed out (Core 6.1 Vol 1 Part F 2.4).
pub(crate) const PAGE_TIMEOUT: u8 = 0x04;
pub(crate) const AVDTP_OPEN: u8 = 0x06;
/// The identifiers of the first control dial `Trace::carry` makes; each later dial takes the next of both.
const DIAL_CIDS: (u16, u16) = (0x0060, 0x0070);
/// How long before its first signal `Trace::carry` opens the signalling channel, and before its first packet the media channel.
const CHANNEL_LEAD_MS: u64 = 20;

impl Trace {
    /// The device at `address` asks the host for an ACL link.
    pub(crate) fn connection_request(&mut self, address: [u8; 6]) {
        let mut parameters = wire(address);
        parameters.extend(AUDIO_CLASS_ON_ACL);
        self.event(EVENT_CONNECTION_REQUEST, &parameters);
    }

    /// The host hears `address` advertise, as a controller that scans with the extended commands reports it.
    pub(crate) fn advert(&mut self, address: [u8; 6], data: &[u8]) {
        let mut parameters = vec![LE_EXTENDED_ADVERTISING_REPORT];
        parameters.extend(ONE_EXTENDED_REPORT);
        parameters.extend(wire(address));
        parameters.extend(EXTENDED_REPORT_REST);
        parameters.push(u8::try_from(data.len()).unwrap());
        parameters.extend(data);
        self.event(EVENT_LE_META, &parameters);
    }

    /// The same, as a controller that scans with the first LE commands reports it.
    pub(crate) fn legacy_advert(&mut self, address: [u8; 6], data: &[u8]) {
        let mut parameters = vec![LE_ADVERTISING_REPORT];
        parameters.extend(ONE_LEGACY_REPORT);
        parameters.extend(wire(address));
        parameters.push(u8::try_from(data.len()).unwrap());
        parameters.extend(data);
        parameters.push(REPORT_RSSI);
        self.event(EVENT_LE_META, &parameters);
    }

    /// What `carried` lists, on the current handle, each part at its own time.
    pub(crate) fn carry(&mut self, carried: &Carried) {
        enum Part {
            Dial(u16),
            Channel((u16, u16)),
            Signal(u8),
            Media,
        }

        let mut parts: Vec<_> = carried
            .set_configurations
            .iter()
            .map(|ms| (*ms, Part::Signal(SET_CONFIGURATION)))
            .chain(carried.open_ms.map(|ms| (ms, Part::Signal(AVDTP_OPEN))))
            .chain(carried.start_ms.map(|ms| (ms, Part::Signal(AVDTP_START))))
            .collect();
        if let Some(ms) = parts.iter().map(|(ms, _)| *ms).min() {
            parts.push((
                ms.saturating_sub(CHANNEL_LEAD_MS),
                Part::Channel(SIGNALLING_CIDS),
            ));
        }
        parts.extend(
            carried
                .control_dials
                .iter()
                .map(|&(ms, result)| (ms, Part::Dial(result))),
        );
        if let Some(ms) = carried.media_ms {
            parts.push((
                ms.saturating_sub(CHANNEL_LEAD_MS),
                Part::Channel(MEDIA_CIDS),
            ));
            parts.push((ms, Part::Media));
        }
        parts.sort_by_key(|(ms, _)| *ms);

        let (host, pods) = (Direction::ToPods, Direction::FromPods);
        let mut cids = DIAL_CIDS;
        let mut label = 1_u8;
        for (ms, part) in parts {
            self.at_ms(ms);
            match part {
                Part::Dial(result) => {
                    self.connect(host, CONTROL_PSM, cids, result);
                    cids = (cids.0.saturating_add(1), cids.1.saturating_add(1));
                }
                Part::Channel(cids) => self.open(host, AVDTP_PSM, cids),
                Part::Signal(signal) => {
                    let command = avdtp(label, COMMAND, signal, &[FIRST_SEID]);
                    self.send(host, SIGNALLING_CIDS, &command);
                    let accepted = avdtp(label, ACCEPT, signal, &[]);
                    self.send(pods, SIGNALLING_CIDS, &accepted);
                    label = label.saturating_add(1);
                }
                Part::Media => {
                    self.send(host, MEDIA_CIDS, &rtp(0));
                    self.send(host, MEDIA_CIDS, &rtp(1));
                }
            }
        }
    }
}

/// What one link carries once it is up; every time is in milliseconds after the trace starts, and a media packet needs a signal before it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Carried {
    /// When the host dials the control channel, and the result the pods answer each dial with.
    pub(crate) control_dials: Vec<(u64, u16)>,
    /// When the host sends each Set Configuration command.
    pub(crate) set_configurations: Vec<u64>,
    pub(crate) open_ms: Option<u64>,
    pub(crate) start_ms: Option<u64>,
    /// When the host sends the first of two media packets.
    pub(crate) media_ms: Option<u64>,
}

/// What the pods' link carries in the first trial of `two_devices`: one dial that opens, one configuration, then the stream.
pub(crate) fn whole_connection() -> Carried {
    Carried {
        control_dials: vec![(1_300, RESULT_SUCCESS)],
        set_configurations: vec![1_400],
        open_ms: Some(1_500),
        start_ms: Some(1_600),
        media_ms: Some(1_700),
    }
}

/// The result of a connection response that refuses a dial for want of resources (Core 6.1 Vol 3 Part A 4.3).
pub(crate) const NO_RESOURCES: u16 = 0x0004;

/// Three trials with a second set of pods near, whose link carries the same identifiers: the pods begin a whole connection that ends, the other pods connect alone, then the host begins a connection whose first dial is refused and that is configured twice and never streams.
pub(crate) fn two_devices() -> Trace {
    let mut trace = Trace::new();
    trace.at_ms(900);
    trace.advert(OTHER_PODS_ADDRESS, &OTHER_APPLE_ADVERT);
    trace.at_ms(1_000);
    trace.advert(RANDOM_ADVERT_ADDRESS, &PROXIMITY_ADVERT);
    trace.at_ms(1_100);
    trace.create_connection(OTHER_PODS_ADDRESS);
    trace.at_ms(1_150);
    trace.link_up(OTHER_PODS_HANDLE, OTHER_PODS_ADDRESS);
    trace.on(OTHER_PODS_HANDLE);
    trace.carry(&Carried {
        control_dials: vec![(1_160, NO_RESOURCES), (1_165, RESULT_SUCCESS)],
        set_configurations: vec![1_190, 1_200, 1_210],
        open_ms: Some(1_215),
        start_ms: Some(1_220),
        media_ms: Some(1_245),
    });
    trace.at_ms(1_250);
    trace.connection_request(PODS_ADDRESS);
    trace.at_ms(1_280);
    trace.link_up(PODS_HANDLE, PODS_ADDRESS);
    trace.on(PODS_HANDLE);
    trace.carry(&whole_connection());
    trace.on(OTHER_PODS_HANDLE);
    trace.carry(&Carried {
        control_dials: vec![(2_000, NO_RESOURCES)],
        ..Carried::default()
    });
    trace.at_ms(9_000);
    trace.end_link(PODS_HANDLE);
    trace.at_ms(9_500);
    trace.end_link(OTHER_PODS_HANDLE);
    trace.at_ms(11_000);
    trace.advert(OTHER_PODS_ADDRESS, &OTHER_APPLE_ADVERT);
    trace.at_ms(11_200);
    trace.connection_request(OTHER_PODS_ADDRESS);
    trace.at_ms(11_250);
    trace.link_up(OTHER_PODS_HANDLE, OTHER_PODS_ADDRESS);
    trace.on(OTHER_PODS_HANDLE);
    trace.carry(&Carried {
        control_dials: vec![(11_300, RESULT_SUCCESS)],
        ..Carried::default()
    });
    trace.at_ms(19_000);
    trace.end_link(OTHER_PODS_HANDLE);
    trace.at_ms(21_000);
    trace.create_connection(PODS_ADDRESS);
    trace.at_ms(21_400);
    trace.link_up(PODS_HANDLE, PODS_ADDRESS);
    trace.on(PODS_HANDLE);
    trace.carry(&Carried {
        control_dials: vec![(21_500, NO_RESOURCES), (22_300, RESULT_SUCCESS)],
        set_configurations: vec![21_600, 21_900],
        ..Carried::default()
    });
    trace
}
