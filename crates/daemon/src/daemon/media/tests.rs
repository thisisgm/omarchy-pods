//! Support tests: which status replies read as playing, whatever a player puts in them, and the adapter's order on a paused clock.

use std::future::{pending, poll_fn};
use std::pin::Pin;
use std::time::Duration;

use pods_core::policy::{Era, MediaRequest, PlayerOwner};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use zbus::export::futures_core::Stream;
use zbus::{Connection, Message, MessageStream, Variant};

use super::{
    PLAYER_CALL_TIMEOUT_MS, PLAYER_PATH, PLAYER_PREFIX, PLAYING, ask_each, queue, reads_playing,
    run, within,
};
use crate::daemon::paused::{on_paused_clock, peers};

/// The one player's unique name on the stand in bus.
const OWNER: &str = ":1.5";

/// A reply to a `Get` call, carrying `body`.
fn reply(body: &(impl zbus::export::serde::Serialize + zbus::DynamicType)) -> Message {
    let call = Message::method_call(PLAYER_PATH, "Get").build(&()).unwrap();
    Message::method_return(&call.header()).build(body).unwrap()
}

#[test]
fn only_the_string_playing_in_a_variant_reads_as_playing() {
    assert!(reads_playing(&reply(&Variant::from(PLAYING))));
    let long = "Playing".repeat(10_000);
    let refused = [
        reply(&Variant::from("Paused")),
        reply(&Variant::from("playing")),
        reply(&Variant::from("Playing ")),
        reply(&Variant::from(long.as_str())),
        reply(&Variant::from(1_u32)),
        reply(&Variant::Variant(Box::new(Variant::from(PLAYING)))),
        reply(&()),
    ];
    for refused in refused {
        assert!(!reads_playing(&refused), "{refused:?}");
    }
}

#[test]
fn a_player_that_never_answers_delays_no_other_player() {
    let answers = on_paused_clock(async {
        let started = Instant::now();
        // Listed first, so asking the players in turn would wait out its timeout before the other.
        let ask = |hangs: bool| async move {
            if hangs {
                within(pending::<()>()).await;
            }
            Some((hangs, started.elapsed()))
        };
        ask_each(vec![true, false], ask).await
    });
    let timeout = Duration::from_millis(PLAYER_CALL_TIMEOUT_MS);
    assert_eq!(answers, [(false, Duration::ZERO), (true, timeout)]);
}

/// The session bus and one playing player in one peer; it holds its status read until `released` and hands each member asked to `asked`.
async fn serve_one_player(
    peer: Connection,
    asked: mpsc::UnboundedSender<String>,
    mut read: Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>,
) {
    let mut calls = MessageStream::from(&peer);
    while let Some(Ok(call)) = poll_fn(|context| Pin::new(&mut calls).poll_next(context)).await {
        let header = call.header();
        let member = header
            .member()
            .map_or_else(String::new, ToString::to_string);
        asked.send(member.clone()).unwrap();
        match member.as_str() {
            "ListNames" => {
                peer.reply(&header, &vec![format!("{PLAYER_PREFIX}one")])
                    .await
            }
            "GetNameOwner" => peer.reply(&header, &OWNER).await,
            "Get" => {
                if let Some((reading, released)) = read.take() {
                    reading.send(()).unwrap();
                    released.await.unwrap();
                }
                peer.reply(&header, &Variant::from(PLAYING)).await
            }
            _ => peer.reply(&header, &()).await,
        }
        .unwrap();
    }
}

#[test]
fn a_loss_during_the_status_read_sends_no_pause_and_the_resume_behind_it_still_plays() {
    let asked = on_paused_clock(async {
        let (session, peer) = peers().await;
        let (asked, mut members) = mpsc::unbounded_channel();
        let (reading, mut read) = oneshot::channel();
        let (release, released) = oneshot::channel();
        tokio::spawn(serve_one_player(peer, asked, Some((reading, released))));
        let (outlet, queue) = queue();
        outlet.offer(MediaRequest::PauseAll(Era::default()));
        outlet.offer(MediaRequest::Play(vec![PlayerOwner::new(OWNER).unwrap()]));
        let (inputs, _taken) = mpsc::channel(1);
        let adapter = tokio::spawn(run(session, inputs, queue));
        (&mut read).await.unwrap();
        outlet.link_lost();
        release.send(()).unwrap();
        drop(outlet);
        adapter.await.unwrap();
        let mut taken = Vec::new();
        while let Ok(member) = members.try_recv() {
            taken.push(member);
        }
        taken
    });
    assert_eq!(asked, ["ListNames", "GetNameOwner", "Get", "Play"]);
}
