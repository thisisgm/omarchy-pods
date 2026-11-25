//! Support tests: what the driver does for dictation, with the driver in the test's own hands.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pods_core::link::{End, Signal};
use pods_core::policy::{DictationFact, DictationRequest, Effect, Fact};
use pods_core::prefs::MicPref;
use pods_core::state::{MicDictation, State};
use pods_fake::pair;
use pods_proto::dictation::DictationPacket;
use pods_proto::doap::{Handles, doap_mode_on};
use pods_samples::doap::{self, host};
use pods_samples::opus;
use pods_samples::uplink::{audio_sdu, silent_unit};
use pods_sys::l2cap::{Channel, ChannelError, Received};
use rustix::io::Errno;
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tokio::task;
use tokio::time::{sleep, timeout};

use super::super::{Connector, Driver, Input, Outlets, REQUEST_QUEUE, heard, queue};
use super::{HANG_LIMIT, device, refusing, unread_outlets};
use crate::daemon::dictation;
use crate::daemon::hub::Hub;

const POLL: Duration = Duration::from_millis(5);
/// Longer than any packet these tests read.
const BUFFER_LEN: usize = 64;
/// The turns the adapter gets for what needs no time to pass, far fewer than fit in its answer time.
const TURNS: usize = 1024;

/// A driver nobody runs, whose attribute channel dials `dictation`; its queue comes back so the queue stays open.
fn driver(dictation: Connector) -> (Driver, mpsc::Receiver<Input>) {
    let (link, inputs) = queue();
    let hub = Hub::new(link.clone(), State::default());
    let (requests, _unread) = mpsc::channel(REQUEST_QUEUE);
    let outlets = Outlets {
        dictation: dictation::Outlet::new(dictation),
        ..unread_outlets()
    };
    let driver = Driver::new(hub, link, requests, refusing(), outlets, None);
    (driver, inputs)
}

/// The next read of the pods' end that is not an empty socket, which must come inside `HANG_LIMIT`.
async fn read(end: &Channel) -> Option<Vec<u8>> {
    let read = timeout(HANG_LIMIT, async {
        let mut buffer = [0_u8; BUFFER_LEN];
        loop {
            match end.try_recv(&mut buffer).unwrap() {
                Received::Sdu(len) => return Some(buffer[..len].to_vec()),
                Received::Closed => return None,
                Received::TimedOut => sleep(POLL).await,
                Received::Truncated { full_len } => panic!("a {full_len} byte packet"),
            }
        }
    });
    read.await
        .unwrap_or_else(|_| panic!("nothing from the host within {HANG_LIMIT:?}"))
}

/// The host closed its end inside `TURNS` turns: no clock is waited on, so the adapter's own answer time is not what closed it.
async fn closed_now(end: &Channel) {
    let mut buffer = [0_u8; BUFFER_LEN];
    for _ in 0..TURNS {
        if end.try_recv(&mut buffer).unwrap() == Received::Closed {
            return;
        }
        task::yield_now().await;
    }
    panic!("the host kept its end for {TURNS} turns");
}

/// The host keeps its end, and writes nothing more, through `TURNS` turns: nothing the adapter does by itself closes it.
async fn kept_open(end: &Channel) {
    let mut buffer = [0_u8; BUFFER_LEN];
    for _ in 0..TURNS {
        assert_eq!(end.try_recv(&mut buffer).unwrap(), Received::TimedOut);
        task::yield_now().await;
    }
}

#[test]
fn the_mode_is_written_on_the_control_channel() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let (mut driver, _inputs) = driver(refusing());
        let (control, end) = pair();
        driver.take(Input::Opened(device(), control));
        driver.route(Effect::Dictation(DictationRequest::Mode));
        assert_eq!(read(&end).await.unwrap(), doap_mode_on());
    });
}

#[test]
fn a_queued_answer_cannot_arm_a_reopened_channel() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let (mut driver, mut inputs) = driver(refusing());
        let (control, _end) = pair();
        driver.take(Input::Opened(device(), control));
        driver.hub.replace(State {
            connected: true,
            ..State::default()
        });
        driver.observe(Fact::Link(Signal::Up { device: device() }));
        let Some(Input::Dictation(previous, DictationFact::BearerFailed)) =
            timeout(HANG_LIMIT, inputs.recv()).await.unwrap()
        else {
            panic!("expected the previous arm's bearer answer");
        };

        for preference in [MicPref::Off, MicPref::Auto] {
            let mut next = driver.hub.state();
            next.microphone.set_preference(preference);
            driver.hub.replace(next);
            driver.observe(Fact::Changed);
        }

        driver.take(Input::Dictation(previous, DictationFact::Armed));
        assert_eq!(driver.hub.state().mic_dictation, MicDictation::Arming);
    });
}

#[test]
fn heard_reads_the_dictation_count() {
    let (mut driver, _inputs) = driver(refusing());
    driver.packet(&audio_sdu(0, &[silent_unit()]));

    let tap = &driver.outlets.tap;
    let frame = opus::tone().next().unwrap();
    for counter in [0, 1] {
        let pdu = doap::audio(
            &doap::Handles::OBSERVED,
            counter,
            &[(u16::from(counter), frame)],
        );
        let Some(DictationPacket::Audio(audio)) =
            DictationPacket::parse(&pdu, &Handles::OBSERVED).unwrap()
        else {
            panic!("expected a dictation audio notification");
        };
        tap.dictation(&audio);
    }

    let Fact::Dictation(DictationFact::Heard { packets }) = heard(tap) else {
        panic!("expected a dictation stall count");
    };
    assert_eq!(packets, 2);
}

#[test]
fn the_adapter_lives_from_the_control_channels_opening_to_its_end() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let (att, att_end) = pair();
        let att = Mutex::new(Some(att));
        let dials = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&dials);
        // Only a dial of the link's own device gets the channel.
        let dial: Connector = Arc::new(move |dialed| {
            counted.fetch_add(1, Ordering::Relaxed);
            let att = att.lock().unwrap().take_if(|_| dialed == device());
            att.ok_or(ChannelError::Connect(Errno::CONNREFUSED))
        });
        let (mut driver, _inputs) = driver(dial);
        // With no link the request has no adapter to go to, so it dials nothing.
        driver.route(Effect::Dictation(DictationRequest::Arm));
        let (control, _control_end) = pair();
        driver.take(Input::Opened(device(), control));
        driver.route(Effect::Dictation(DictationRequest::Arm));
        // The arm's first request: the adapter runs, took the request and dialed the attribute channel.
        assert_eq!(read(&att_end).await.unwrap(), host::declarations_from(1));
        kept_open(&att_end).await;
        driver.take(Input::Ended(End::Closed));
        closed_now(&att_end).await;
        assert_eq!(dials.load(Ordering::Relaxed), 1);
    });
}
