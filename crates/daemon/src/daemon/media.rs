//! Pauses and resumes the desktop's media players over the session bus, as the policy asks; it keeps no watch between requests.

use std::future::poll_fn;
use std::ops::ControlFlow;
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use pods_core::policy::{MediaRequest, PAUSED_MAX, PlayerOwner};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::watch;
use tokio::time::timeout;
use zbus::export::serde::Serialize;
use zbus::names::OwnedUniqueName;
use zbus::{Connection, DynamicType, Message, Variant, fdo};

use super::link::Input;
use super::log;

/// A judgment: more players than this is a hostile bus, and the policy remembers as many.
pub(super) const PLAYERS_MAX: usize = PAUSED_MAX;

/// A judgment: a pause later than this is no longer the gesture's effect.
pub(super) const PLAYER_CALL_TIMEOUT_MS: u64 = 500;

/// One request per ear change; a burst past this while a player hangs is dropped with a log line.
pub(super) const REQUEST_QUEUE: usize = 4;

const BUS: &str = "org.freedesktop.DBus";

const BUS_PATH: &str = "/org/freedesktop/DBus";

const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";

const PLAYER_PREFIX: &str = "org.mpris.MediaPlayer2.";

const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

const PLAYING: &str = "Playing";

const QUEUE_FULL: &str = "media: request queue full";

const ADAPTER_GONE: &str = "media: the adapter has ended; request dropped";

/// A request and the count of link losses when it was made, so the pause work of an ended link is dropped.
#[derive(Debug)]
struct Stamped {
    losses: u64,
    request: MediaRequest,
}

/// The driver's end: it offers requests and counts the link's losses.
#[derive(Debug)]
pub(super) struct Outlet {
    requests: mpsc::Sender<Stamped>,
    losses: watch::Sender<u64>,
}

/// The adapter's end.
#[derive(Debug)]
pub(super) struct Queue {
    requests: mpsc::Receiver<Stamped>,
    losses: watch::Receiver<u64>,
}

/// The one place the queue's bound is set.
pub(super) fn queue() -> (Outlet, Queue) {
    let (requests, received) = mpsc::channel(REQUEST_QUEUE);
    let (losses, counted) = watch::channel(0);
    let outlet = Outlet { requests, losses };
    let queue = Queue {
        requests: received,
        losses: counted,
    };
    (outlet, queue)
}

impl Outlet {
    /// Queues `request` without waiting; a full queue or an ended adapter drops it with a log line.
    pub(super) fn offer(&self, request: MediaRequest) {
        let losses = *self.losses.borrow();
        match self.requests.try_send(Stamped { losses, request }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => log(QUEUE_FULL),
            // The adapter ends on its own only after the driver, so a live driver sees this only after a panic.
            Err(TrySendError::Closed(_)) => log(ADAPTER_GONE),
        }
    }

    /// Ends every pause made so far, queued or in flight, before it sends another call.
    pub(super) fn link_lost(&self) {
        self.losses
            .send_modify(|losses| *losses = losses.wrapping_add(1));
    }
}

impl Queue {
    /// Drops every request until the driver drops its end, for a daemon with no session bus.
    pub(super) async fn drain(&mut self) {
        while self.requests.recv().await.is_some() {}
    }
}

/// Serves each request in turn until the driver drops the queue or stops hearing facts.
pub(super) async fn run(session: Connection, inputs: mpsc::Sender<Input>, mut queue: Queue) {
    while let Some(Stamped { losses, request }) = queue.requests.recv().await {
        // Only pause work ends with the link, as its report could no longer be recorded; a resume decided while the link was up always goes out.
        let pause = matches!(request, MediaRequest::PauseAll);
        let served = serve(&session, &inputs, request);
        let flow = if pause {
            unless_lost(&mut queue.losses, losses, served).await
        } else {
            Some(served.await)
        };
        if flow == Some(ControlFlow::Break(())) {
            return;
        }
    }
}

/// Runs `work` until the link is lost after `losses`; a loss already counted wins, so no call and no report follows it.
async fn unless_lost<T>(
    counted: &mut watch::Receiver<u64>,
    losses: u64,
    work: impl Future<Output = T>,
) -> Option<T> {
    let mut lost = pin!(counted.wait_for(|now| *now != losses));
    let mut work = pin!(work);
    poll_fn(|context| {
        // An ended driver is a lost link too: nobody is left to take the result.
        if lost.as_mut().poll(context).is_ready() {
            return Poll::Ready(None);
        }
        work.as_mut().poll(context).map(Some)
    })
    .await
}

/// One request; `Break` when the driver no longer hears facts.
async fn serve(
    session: &Connection,
    inputs: &mpsc::Sender<Input>,
    request: MediaRequest,
) -> ControlFlow<()> {
    match request {
        MediaRequest::PauseAll => {
            let paused = pause_playing(session).await;
            if !paused.is_empty() && inputs.send(Input::Media(paused)).await.is_err() {
                return ControlFlow::Break(());
            }
        }
        MediaRequest::Play(players) => {
            let answers = ask_each(players, |player| play(session, player)).await;
            let count = |kind| answers.iter().filter(|&&answer| answer == kind).count();
            let (refused, silent) = (count(Resumed::Refused), count(Resumed::Silent));
            let asked = answers.len().saturating_sub(count(Resumed::Gone));
            if refused > 0 || silent > 0 {
                log(&format!(
                    "media: {refused} of {asked} players refused the resume and {silent} did not answer within {PLAYER_CALL_TIMEOUT_MS} ms"
                ));
            }
        }
    }
    ControlFlow::Continue(())
}

/// The owners of the players that read `Playing` and took the pause.
async fn pause_playing(session: &Connection) -> Vec<PlayerOwner> {
    let players = match list_players(session).await {
        Ok(players) => players,
        Err(error) => {
            log(&format!(
                "media: cannot list the session bus's names: {error}"
            ));
            return Vec::new();
        }
    };
    ask_each(players, |player| pause_if_playing(session, player)).await
}

/// The first `PLAYERS_MAX` names under the MPRIS prefix, with one line when there are more.
async fn list_players(session: &Connection) -> Result<Vec<String>, String> {
    let call = session.call_method(Some(BUS), BUS_PATH, Some(BUS), "ListNames", &());
    let reply = within(call)
        .await
        .ok_or_else(|| format!("no answer within {PLAYER_CALL_TIMEOUT_MS} ms"))?
        .map_err(|error| error.to_string())?;
    // A `ListNames` reply: ["org.freedesktop.DBus", ":1.42", "org.mpris.MediaPlayer2.spotify"]
    let names: Vec<String> = reply
        .body()
        .deserialize()
        .map_err(|error| error.to_string())?;
    let mut players: Vec<String> = names
        .into_iter()
        .filter(|name| {
            name.strip_prefix(PLAYER_PREFIX)
                .is_some_and(|rest| !rest.is_empty())
        })
        .collect();
    if players.len() > PLAYERS_MAX {
        log(&format!(
            "media: {} players on the session bus; only the first {PLAYERS_MAX} are asked",
            players.len()
        ));
        players.truncate(PLAYERS_MAX);
    }
    Ok(players)
}

/// Asks every player at once inside this task and gives the `Some` answers; each ask bounds its own calls.
async fn ask_each<P, T, F: Future<Output = Option<T>>>(
    players: Vec<P>,
    ask: impl Fn(P) -> F,
) -> Vec<T> {
    let mut calls: Vec<_> = players
        .into_iter()
        .map(|player| Box::pin(ask(player)))
        .collect();
    let mut took = Vec::new();
    poll_fn(|context| {
        calls.retain_mut(|call| match call.as_mut().poll(context) {
            Poll::Ready(answer) => {
                took.extend(answer);
                false
            }
            Poll::Pending => true,
        });
        if calls.is_empty() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    took
}

/// Any error from the bus or the player means it did not take the pause; every call goes to the owner, which cannot change hands.
async fn pause_if_playing(session: &Connection, name: String) -> Option<PlayerOwner> {
    let owner = within(owner_of(session, name)).await??;
    let status = (PLAYER_INTERFACE, "PlaybackStatus");
    let reply = within(call(session, &owner, PROPERTIES, "Get", &status))
        .await?
        .ok()?;
    if !reads_playing(&reply) {
        return None;
    }
    // A pause sent but answered late can still land, so the resume asks that player too.
    match within(call(session, &owner, PLAYER_INTERFACE, "Pause", &())).await {
        Some(Err(_)) => None,
        Some(Ok(_)) | None => Some(owner),
    }
}

/// The connection that owns `name` now; a name released meanwhile has none.
async fn owner_of(session: &Connection, name: String) -> Option<PlayerOwner> {
    let call = session.call_method(Some(BUS), BUS_PATH, Some(BUS), "GetNameOwner", &name);
    // A `GetNameOwner` reply: ":1.42"
    let owner: OwnedUniqueName = call.await.ok()?.body().deserialize().ok()?;
    PlayerOwner::new(owner.as_str())
}

/// How one player answered a `Play`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resumed {
    Took,
    Refused,
    Silent,
    /// The connection that took the pause has left, so a player reopened under its name is not started.
    Gone,
}

async fn play(session: &Connection, owner: PlayerOwner) -> Option<Resumed> {
    let answer = match within(call(session, &owner, PLAYER_INTERFACE, "Play", &())).await {
        Some(Ok(_)) => Resumed::Took,
        Some(Err(error)) if left_the_bus(&error) => Resumed::Gone,
        Some(Err(_)) => Resumed::Refused,
        None => Resumed::Silent,
    };
    Some(answer)
}

/// A unique name whose connection has left: `ServiceUnknown`, or `NameHasNoOwner` from `dbus-broker` when auto start is off.
fn left_the_bus(error: &zbus::Error) -> bool {
    let zbus::Error::FDO(error) = error else {
        return false;
    };
    matches!(
        **error,
        fdo::Error::ServiceUnknown(_) | fdo::Error::NameHasNoOwner(_)
    )
}

async fn call(
    session: &Connection,
    owner: &PlayerOwner,
    interface: &str,
    member: &str,
    body: &(impl Serialize + DynamicType),
) -> zbus::Result<Message> {
    let destination = Some(owner.as_str());
    session
        .call_method(destination, PLAYER_PATH, Some(interface), member, body)
        .await
}

/// The reply to `Get`, a variant holding the string `Playing`; any other type or text is not playing.
fn reads_playing(reply: &Message) -> bool {
    let body = reply.body();
    body.deserialize::<Variant<'_>>()
        .is_ok_and(|status| <&str>::try_from(&status).is_ok_and(|status| status == PLAYING))
}

async fn within<T>(work: impl Future<Output = T>) -> Option<T> {
    timeout(Duration::from_millis(PLAYER_CALL_TIMEOUT_MS), work)
        .await
        .ok()
}

#[cfg(test)]
mod tests;
