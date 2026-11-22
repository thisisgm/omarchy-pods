//! Support tests: the watcher's own guard on a call, for a race no end to end test can order.

use std::future::poll_fn;
use std::os::unix::net::UnixStream;
use std::pin::Pin;
use std::time::Duration;

use pods_core::link::{Call, DeviceAction, Session};
use pods_proto::addr::BdAddr;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::mpsc;
use tokio::time::timeout;
use zbus::connection::Builder;
use zbus::export::futures_core::Stream;
use zbus::{Guid, MessageStream};

use super::calls::Calls;
use super::objects::{Candidates, Change, Properties};
use super::{DeviceEvent, ErrorLog, Input, Watcher};

/// Long enough for a call the watcher wrongly made to reach the peer.
const QUIET: Duration = Duration::from_millis(300);

const PODS: &str = "/org/bluez/hci0/dev_02_00_00_00_00_01";

#[test]
fn a_call_for_pods_removed_after_the_machine_chose_them_fails_without_a_call() {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (watcher_end, peer_end) = UnixStream::pair().unwrap();
        let peer = tokio::spawn(
            Builder::unix_stream(peer_end)
                .server(Guid::generate())
                .p2p()
                .build(),
        );
        let connection = Builder::unix_stream(watcher_end)
            .p2p()
            .build()
            .await
            .unwrap();
        let peer = peer.await.unwrap().unwrap();
        let mut reaching_peer = MessageStream::from(&peer);
        let device = BdAddr::from([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let mut candidates = Candidates::default();
        let pods = Properties {
            address: Some(device),
            lists_aap: Some(true),
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
        let mut errors = ErrorLog::default();
        let mut calls = Calls::default();
        let mut watcher = Watcher {
            connection,
            inputs: &inputs,
            candidates,
            errors: &mut errors,
            retry: None,
            calls: &mut calls,
        };
        let call = Call {
            action: DeviceAction::Connect,
            session: Session::default(),
        };
        assert!(watcher.call(device, call).await.is_continue());
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
