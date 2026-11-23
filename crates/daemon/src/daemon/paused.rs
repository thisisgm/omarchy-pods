//! Test support: a future run on tokio's paused clock, where a wait that never ends fails instead of parking the test, and a bus to run there.

use std::future::Future;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use tokio::runtime::Builder;
use tokio::time::timeout;
use zbus::connection::Builder as BusBuilder;
use zbus::{Connection, Guid};

/// Bounds a whole test's paused time, twice the most any test here spends (the 30 s BlueZ call); it costs nothing.
const PAUSED_HANG_LIMIT: Duration = Duration::from_secs(60);

/// Runs `work` to its end on a current thread runtime whose clock starts paused.
pub(in crate::daemon) fn on_paused_clock<F: Future>(work: F) -> F::Output {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    runtime
        .block_on(async { timeout(PAUSED_HANG_LIMIT, work).await })
        .unwrap_or_else(|_| {
            panic!("the work did not end within {PAUSED_HANG_LIMIT:?} of paused time")
        })
}

/// The daemon's end of a peer to peer bus, and the peer's.
pub(in crate::daemon) async fn peers() -> (Connection, Connection) {
    let (daemon_end, peer_end) = UnixStream::pair().unwrap();
    let peer = tokio::spawn(
        BusBuilder::unix_stream(peer_end)
            .server(Guid::generate())
            .p2p()
            .build(),
    );
    let connection = BusBuilder::unix_stream(daemon_end)
        .p2p()
        .build()
        .await
        .unwrap();
    (connection, peer.await.unwrap().unwrap())
}
