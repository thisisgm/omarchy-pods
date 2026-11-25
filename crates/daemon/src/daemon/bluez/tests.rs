//! Support tests: the watcher's own guard on a call, for a race no end to end test can order, and its waits on a paused clock.

use std::future::poll_fn;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use pods_core::link::{Call, DeviceAction, Session};
use pods_proto::addr::BdAddr;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep, timeout};
use zbus::export::futures_core::Stream;
use zbus::{Connection, Message, MessageStream};

use super::calls::{self, Calls};
use super::monitor::Monitor;
use super::objects::{Candidates, Change, Kind, Properties};
use super::{DeviceEvent, ErrorLog, Input, Request, Watcher, failure, wait_to_retry};
use crate::daemon::paused::{on_paused_clock, peers};

/// Long enough for a call the watcher wrongly made to reach the peer.
const QUIET: Duration = Duration::from_millis(300);

/// `SYSTEM_BUS_RETRY_MS`, written out so a drifted retry goes red.
const BUS_RETRY: Duration = Duration::from_millis(5000);

/// `CALL_TIMEOUT_MS` of the calls, written out like `BUS_RETRY`.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// `SWEEP_RETRY_MS`, written out like `BUS_RETRY`.
const SWEEP_RETRY: Duration = Duration::from_millis(1000);

/// Inside the retry pause, when the driver asks for a sweep.
const SWEEP_ASKED: Duration = Duration::from_millis(1000);

const PODS: &str = "/org/bluez/hci0/dev_02_00_00_00_00_01";

#[test]
fn failure_logs_only_the_dbus_error_name() {
    let description = "No such object /org/bluez/hci0/dev_00_11_22_33_44_55";
    let expected = "org.freedesktop.DBus.Error.UnknownObject";
    let request = Message::method_call("/", "Connect").build(&()).unwrap();
    let reply = Message::error(&request.header(), "org.bluez.Error.Failed")
        .build(&(description,))
        .unwrap();
    let method_error = zbus::Error::from(reply);
    let unknown_object = zbus::fdo::Error::UnknownObject(description.to_owned());
    let fdo_error = zbus::Error::from(unknown_object.clone());
    let dbus_error = zbus::Error::DBus(Arc::new(unknown_object));

    assert_eq!(failure(&method_error), "org.bluez.Error.Failed");
    assert_eq!(failure(&fdo_error), expected);
    assert_eq!(failure(&dbus_error), expected);
}

#[test]
fn failure_omits_the_dbus_handshake_description() {
    let error = zbus::Error::Handshake("Unexpected reply /org/bluez/hci0".to_owned());
    assert_eq!(failure(&error), "D-Bus handshake failed");
}

/// What a watcher borrows across connections, kept by the test.
#[derive(Default)]
struct Kept {
    errors: ErrorLog,
    calls: Calls,
    monitor: Monitor,
}

impl Kept {
    fn watcher<'a>(
        &'a mut self,
        connection: Connection,
        inputs: &'a mpsc::Sender<Input>,
        candidates: Candidates,
    ) -> Watcher<'a> {
        Watcher {
            connection,
            inputs,
            candidates,
            errors: &mut self.errors,
            retry: None,
            calls: &mut self.calls,
            monitor: &mut self.monitor,
        }
    }
}

#[test]
fn a_call_for_pods_removed_after_the_machine_chose_them_fails_without_a_call() {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (connection, peer) = peers().await;
        let mut reaching_peer = MessageStream::from(&peer);
        let device = BdAddr::from([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let mut candidates = Candidates::default();
        let pods = Properties {
            address: Some(device),
            kind: Some(Kind::Pods),
            connected: Some(true),
        };
        candidates.apply(Change::Device {
            path: PODS.to_owned(),
            properties: pods,
        });
        // The machine chose these pods; BlueZ's removal reaches the watcher before its call does.
        candidates.apply(Change::Removed {
            path: PODS.to_owned(),
        });
        let (inputs, mut events) = mpsc::channel(1);
        let mut kept = Kept::default();
        let mut watcher = kept.watcher(connection, &inputs, candidates);
        let call = Call {
            action: DeviceAction::Connect,
            session: Session::default(),
        };
        assert!(watcher.call(device, call, false).await.is_continue());
        let event = events.try_recv();
        assert!(
            matches!(event, Ok(Input::Device(DeviceEvent::CallFailed(failed))) if failed == call),
            "{event:?}"
        );
        let reached = poll_fn(|context| Pin::new(&mut reaching_peer).poll_next(context));
        assert!(
            timeout(QUIET, reached).await.is_err(),
            "a message reached the peer"
        );
    });
}

#[test]
fn a_lost_bus_is_dialled_again_the_retry_after_even_when_a_sweep_is_asked_meanwhile() {
    let waited = on_paused_clock(async {
        let (inputs, _driver) = mpsc::channel(1);
        let (asks, mut requests) = mpsc::channel(1);
        let (_wants, mut wanted) = watch::channel(false);
        tokio::spawn(async move {
            sleep(SWEEP_ASKED).await;
            asks.send(Request::Sweep).await.unwrap();
            // Held open, so the pause never reads a driver that is gone.
            std::future::pending::<()>().await;
        });
        let started = Instant::now();
        let flow = wait_to_retry(
            &inputs,
            &mut requests,
            &mut wanted,
            &mut Calls::default(),
            &mut Monitor::default(),
        )
        .await;
        assert_eq!(flow, ControlFlow::Continue(()));
        started.elapsed()
    });
    assert_eq!(waited, BUS_RETRY);
}

#[test]
fn a_call_bluez_never_answers_fails_after_the_call_timeout() {
    let (waited, made) = on_paused_clock(async {
        let (connection, _peer) = peers().await;
        let started = Instant::now();
        let made = calls::run(connection, PODS.to_owned(), DeviceAction::Connect, false).await;
        (started.elapsed(), made)
    });
    assert_eq!(waited, CALL_TIMEOUT);
    assert!(made.is_err(), "the call was answered");
}

#[test]
fn a_sweep_bluez_refuses_at_once_arms_the_sweep_retry() {
    let (spent, waits) = on_paused_clock(async {
        let (connection, peer) = peers().await;
        let mut calls = MessageStream::from(&peer);
        // BlueZ owns its name on the bus but fails every sweep.
        tokio::spawn(async move {
            while let Some(Ok(call)) =
                poll_fn(|context| Pin::new(&mut calls).poll_next(context)).await
            {
                let header = call.header();
                if header
                    .member()
                    .is_some_and(|member| member == "NameHasOwner")
                {
                    peer.reply(&header, &true).await.unwrap();
                } else {
                    peer.reply_error(&header, "org.bluez.Error.Failed", &())
                        .await
                        .unwrap();
                }
            }
        });
        let (inputs, _events) = mpsc::channel(1);
        let mut kept = Kept::default();
        let mut watcher = kept.watcher(connection, &inputs, Candidates::default());
        let started = Instant::now();
        assert!(watcher.sweep().await.is_continue());
        let retry = watcher.retry.as_ref().map(|retry| retry.deadline());
        (
            started.elapsed(),
            retry.map(|due| due.duration_since(Instant::now())),
        )
    });
    assert_eq!(spent, Duration::ZERO, "the sweep waited out its call");
    assert_eq!(waits, Some(SWEEP_RETRY));
}
