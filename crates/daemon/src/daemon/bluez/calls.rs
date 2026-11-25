//! The BlueZ calls behind connect, disconnect and forget, each its own task.

use std::collections::HashMap;
use std::task::{Context, Poll};
use std::time::Duration;

use pods_core::link::{Call, DeviceAction};
use tokio::task::{Id, JoinSet};
use tokio::time::timeout;
use zbus::{Connection, ObjectPath, OwnedObjectPath, OwnedVariant};

use super::{ADAPTER_INTERFACE, BLUEZ, DEVICE_INTERFACE, PROPERTIES};

/// A BlueZ `Connect` can run for a whole page and the set up of every profile.
const CALL_TIMEOUT_MS: u64 = 30_000;

/// Past this, the next call fails at once.
const CALLS_IN_FLIGHT_MAX: usize = 4;

#[derive(Debug, Default)]
pub(super) struct Calls {
    tasks: JoinSet<zbus::Result<()>>,
    made: HashMap<Id, Call>,
}

impl Calls {
    /// False, with nothing started, while `CALLS_IN_FLIGHT_MAX` are under way.
    #[must_use]
    pub(super) fn start(
        &mut self,
        connection: &Connection,
        path: String,
        call: Call,
        when_worn: bool,
    ) -> bool {
        if self.tasks.len() >= CALLS_IN_FLIGHT_MAX {
            return false;
        }
        let task = self
            .tasks
            .spawn(run(connection.clone(), path, call.action, when_worn));
        self.made.insert(task.id(), call);
        true
    }

    /// `Pending` while none has ended, or none is under way.
    pub(super) fn poll_ended(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<(Call, zbus::Result<()>)> {
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
        match self.made.remove(&id) {
            Some(call) => Poll::Ready((call, result)),
            None => Poll::Pending,
        }
    }
}

fn method(action: DeviceAction) -> &'static str {
    match action {
        DeviceAction::Connect => "Connect",
        DeviceAction::Disconnect => "Disconnect",
        DeviceAction::Forget => "RemoveDevice",
    }
}

pub(super) async fn run(
    connection: Connection,
    path: String,
    action: DeviceAction,
    when_worn: bool,
) -> Result<(), zbus::Error> {
    let call = async {
        if when_worn {
            require_paired(&connection, &path).await?;
            crate::daemon::log("link: connect when worn");
        }
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

// Sample input: `variant boolean true`, the reply to `Get` of `Paired`.
async fn require_paired(connection: &Connection, path: &str) -> zbus::Result<()> {
    let reply = connection
        .call_method(
            Some(BLUEZ),
            path,
            Some(PROPERTIES),
            "Get",
            &(DEVICE_INTERFACE, "Paired"),
        )
        .await?;
    let paired = reply
        .body()
        .deserialize::<OwnedVariant>()
        .ok()
        .and_then(|value| bool::try_from(value).ok());
    match paired {
        Some(true) => Ok(()),
        Some(false) => Err(zbus::Error::Failure(
            "BlueZ did not confirm pairing".to_owned(),
        )),
        None => Err(zbus::Error::Failure(
            "BlueZ Paired reply was not a boolean".to_owned(),
        )),
    }
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
