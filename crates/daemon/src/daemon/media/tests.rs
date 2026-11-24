//! Support tests: which status replies read as playing, whatever a player puts in them, and the adapter's order and reports on a paused clock.

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
use crate::daemon::link::Input;
use crate::daemon::paused::{on_paused_clock, peers};

/// The one player's unique name on the stand in bus.
const OWNER: &str = ":1.5";

/// Room for both reports of one pause, so an adapter that wrongly told a cut would not block on a full queue.
const REPORTS_OF_A_CUT_PAUSE: usize = 2;

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

/// A member whose first call is held: the peer tells it came, then answers it once released.
type Held = (&'static str, oneshot::Sender<()>, oneshot::Receiver<()>);

/// The session bus and one playing player in one peer; it holds the first call of `held`'s member and hands each member asked to `asked`.
async fn serve_one_player(
    peer: Connection,
    asked: mpsc::UnboundedSender<String>,
    mut held: Option<Held>,
) {
    let mut calls = MessageStream::from(&peer);
    while let Some(Ok(call)) = poll_fn(|context| Pin::new(&mut calls).poll_next(context)).await {
        let header = call.header();
        let member = header
            .member()
            .map_or_else(String::new, ToString::to_string);
        asked.send(member.clone()).unwrap();
        if let Some((_, came, released)) = held.take_if(|held| held.0 == member) {
            came.send(()).unwrap();
            released.await.unwrap();
        }
        match member.as_str() {
            "ListNames" => {
                peer.reply(&header, &vec![format!("{PLAYER_PREFIX}one")])
                    .await
            }
            "GetNameOwner" => peer.reply(&header, &OWNER).await,
            "Get" => peer.reply(&header, &Variant::from(PLAYING)).await,
            _ => peer.reply(&header, &()).await,
        }
        .unwrap();
    }
}

#[test]
fn a_loss_during_the_status_read_sends_no_pause_and_the_resume_behind_it_still_plays() {
    let (asked, cut) = on_paused_clock(async {
        let (session, peer) = peers().await;
        let (asked, mut members) = mpsc::unbounded_channel();
        let (reading, mut read) = oneshot::channel();
        let (release, released) = oneshot::channel();
        tokio::spawn(serve_one_player(
            peer,
            asked,
            Some(("Get", reading, released)),
        ));
        let (outlet, queue) = queue();
        outlet.offer(MediaRequest::PauseAll(Era::default()));
        outlet.offer(MediaRequest::Play(vec![PlayerOwner::new(OWNER).unwrap()]));
        let (inputs, mut told) = mpsc::channel(1);
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
        let cut = matches!(told.try_recv(), Ok(Input::PauseCut(era)) if era == Era::default());
        (taken, cut && told.try_recv().is_err())
    });
    assert_eq!(asked, ["ListNames", "GetNameOwner", "Get", "Play"]);
    assert!(cut, "the cut pause was not the one report");
}

#[test]
fn a_pause_that_no_loss_cut_reports_its_player_and_nothing_more() {
    let paused = on_paused_clock(async {
        let (session, peer) = peers().await;
        let (asked, _members) = mpsc::unbounded_channel();
        tokio::spawn(serve_one_player(peer, asked, None));
        let (outlet, queue) = queue();
        outlet.offer(MediaRequest::PauseAll(Era::default()));
        let (inputs, mut told) = mpsc::channel(REPORTS_OF_A_CUT_PAUSE);
        drop(outlet);
        run(session, inputs, queue).await;
        let paused = matches!(told.try_recv(), Ok(Input::Media(_, players)) if players == [PlayerOwner::new(OWNER).unwrap()]);
        paused && told.try_recv().is_err()
    });
    assert!(paused, "the paused player was not the one report");
}

#[test]
fn a_loss_during_the_pause_call_reports_its_player_and_then_the_cut() {
    let told = on_paused_clock(async {
        let (session, peer) = peers().await;
        let (asked, _members) = mpsc::unbounded_channel();
        let (pausing, mut pause) = oneshot::channel();
        let (release, released) = oneshot::channel();
        tokio::spawn(serve_one_player(
            peer,
            asked,
            Some(("Pause", pausing, released)),
        ));
        let (outlet, queue) = queue();
        outlet.offer(MediaRequest::PauseAll(Era::default()));
        let (inputs, mut told) = mpsc::channel(REPORTS_OF_A_CUT_PAUSE);
        let adapter = tokio::spawn(run(session, inputs, queue));
        (&mut pause).await.unwrap();
        outlet.link_lost();
        release.send(()).unwrap();
        drop(outlet);
        adapter.await.unwrap();
        let paused = matches!(told.try_recv(), Ok(Input::Media(_, players)) if players == [PlayerOwner::new(OWNER).unwrap()]);
        let cut = matches!(told.try_recv(), Ok(Input::PauseCut(era)) if era == Era::default());
        paused && cut && told.try_recv().is_err()
    });
    assert!(
        told,
        "the paused player and then the cut were not the two reports"
    );
}
