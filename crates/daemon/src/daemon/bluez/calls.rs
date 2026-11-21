//! The BlueZ method calls behind connect, disconnect and forget, each a task of the watcher's own.

use std::collections::HashMap;
use std::task::{Context, Poll};
use std::time::Duration;

use pods_core::link::DeviceAction;
use tokio::task::{Id, JoinSet};
use tokio::time::timeout;
use zbus::{Connection, ObjectPath, OwnedObjectPath, OwnedVariant};

use super::{BLUEZ, DEVICE_INTERFACE, PROPERTIES};

/// A BlueZ `Connect` can run for a whole page and the set up of every profile.
const CALL_TIMEOUT_MS: u64 = 30_000;

/// BlueZ method calls not yet answered; the next one fails at once.
const CALLS_IN_FLIGHT_MAX: usize = 4;

const ADAPTER_INTERFACE: &str = "org.bluez.Adapter1";

/// The calls under way, at most `CALLS_IN_FLIGHT_MAX`, and the action each carries out.
#[derive(Debug, Default)]
pub(super) struct Calls {
    tasks: JoinSet<zbus::Result<()>>,
    actions: HashMap<Id, DeviceAction>,
}

impl Calls {
    /// Starts `action` on the device at `path`; false, with nothing started, while `CALLS_IN_FLIGHT_MAX` are under way.
    #[must_use]
    pub(super) fn start(
        &mut self,
        connection: &Connection,
        path: String,
        action: DeviceAction,
    ) -> bool {
        if self.tasks.len() >= CALLS_IN_FLIGHT_MAX {
            return false;
        }
        let task = self.tasks.spawn(run(connection.clone(), path, action));
        self.actions.insert(task.id(), action);
        true
    }

    /// The next call that ended; `Pending` while none has, or none is under way.
    pub(super) fn poll_ended(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<(DeviceAction, zbus::Result<()>)> {
        let (id, result) = match self.tasks.poll_join_next_with_id(context) {
            Poll::Ready(Some(Ok(ended))) => ended,
            Poll::Ready(Some(Err(error))) => (
                error.id(),
                Err(zbus::Error::Failure(format!(
                    "the call's task ended: {error}"
                ))),
            ),
            Poll::Ready(None) | Poll::Pending => return Poll::Pending,
        };
        // Every task's id was kept when it was spawned, so none is missing.
        match self.actions.remove(&id) {
            Some(action) => Poll::Ready((action, result)),
            None => Poll::Pending,
        }
    }
}

/// The BlueZ method that carries out `action`.
fn method(action: DeviceAction) -> &'static str {
    match action {
        DeviceAction::Connect => "Connect",
        DeviceAction::Disconnect => "Disconnect",
        DeviceAction::Forget => "RemoveDevice",
    }
}

/// Carries out `action` on the device at `path` within `CALL_TIMEOUT_MS`.
pub(super) async fn run(
    connection: Connection,
    path: String,
    action: DeviceAction,
) -> Result<(), zbus::Error> {
    let call = async {
        match action {
            DeviceAction::Connect | DeviceAction::Disconnect => {
                let device_method = connection.call_method(
                    Some(BLUEZ),
                    path.as_str(),
                    Some(DEVICE_INTERFACE),
                    method(action),
                    &(),
                );
                device_method.await.map(drop)
            }
            DeviceAction::Forget => remove(&connection, &path).await,
        }
    };
    timeout(Duration::from_millis(CALL_TIMEOUT_MS), call)
        .await
        .unwrap_or_else(|_| {
            Err(zbus::Error::Failure(format!(
                "no answer within {CALL_TIMEOUT_MS} ms"
            )))
        })
}

/// `RemoveDevice` on the adapter the device's `Adapter` property names.
async fn remove(connection: &Connection, path: &str) -> zbus::Result<()> {
    let reply = connection
        .call_method(
            Some(BLUEZ),
            path,
            Some(PROPERTIES),
            "Get",
            &(DEVICE_INTERFACE, "Adapter"),
        )
        .await?;
    // Sample input: `<objectpath "/org/bluez/hci0">`, the reply to `Get` of `Adapter`.
    let adapter = OwnedObjectPath::try_from(reply.body().deserialize::<OwnedVariant>()?)?;
    let device = ObjectPath::try_from(path)?;
    connection
        .call_method(
            Some(BLUEZ),
            &adapter,
            Some(ADAPTER_INTERFACE),
            method(DeviceAction::Forget),
            &(device,),
        )
        .await
        .map(drop)
}
