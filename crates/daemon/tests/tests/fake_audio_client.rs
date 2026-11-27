//! A native protocol client for tests driving the fake server.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::support::audio::protocol::{
    AuthParams, AuthReply, Command, CommandReply, DESCRIPTOR_SIZE, MAX_VERSION, Prop, Props,
    ProtocolError, PulseError, SetClientNameReply, SubscriptionEvent, SubscriptionMask,
    read_ack_message, read_command_message, read_reply_message, write_command_message,
};
use crate::support::audio::wire::read_frame;

use super::{ANSWER_WITHIN, COOKIE_LENGTH};

/// One connection to the fake, speaking only through native protocol messages.
pub(super) struct Client {
    pub(super) stream: UnixStream,
    seq: u32,
}

impl Client {
    pub(super) fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).unwrap();
        stream.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();
        Self { stream, seq: 0 }
    }

    /// Connected, authorised and named, as every client of the audio server opens.
    pub(super) fn open(socket: &Path) -> Self {
        let mut client = Self::connect(socket);
        let auth = Command::Auth(AuthParams {
            version: MAX_VERSION,
            cookie: vec![0; COOKIE_LENGTH],
            ..AuthParams::default()
        });
        let reply: AuthReply = client.ask(&auth);
        assert_eq!(reply.version, MAX_VERSION);
        let mut props = Props::new();
        props.set(Prop::ApplicationName, c"fake audio test");
        let _: SetClientNameReply = client.ask(&Command::SetClientName(props));
        client
    }

    pub(super) fn send(&mut self, command: &Command) {
        self.seq = self.seq.wrapping_add(1);
        let mut bytes = Vec::new();
        write_command_message(&mut bytes, self.seq, command).unwrap();
        self.stream.write_all(&bytes).unwrap();
    }

    pub(super) fn frame(&mut self) -> Vec<u8> {
        read_frame(&mut self.stream).unwrap()
    }

    pub(super) fn reply<R: CommandReply>(&mut self) -> R {
        let frame = self.frame();
        self.decode_reply(&frame)
    }

    fn decode_reply<R: CommandReply>(&self, frame: &[u8]) -> R {
        let (seq, reply) = read_reply_message(frame, MAX_VERSION).unwrap();
        assert_eq!(seq, self.seq, "a reply to another command");
        reply
    }

    pub(super) fn ask<R: CommandReply>(&mut self, command: &Command) -> R {
        self.send(command);
        self.reply()
    }

    pub(super) fn ask_with_body<R: CommandReply>(&mut self, command: &Command) -> (R, Vec<u8>) {
        const COMMAND_AND_SEQUENCE: usize = 10;
        self.send(command);
        let frame = self.frame();
        let reply = self.decode_reply(&frame);
        (
            reply,
            frame[DESCRIPTOR_SIZE + COMMAND_AND_SEQUENCE..].to_vec(),
        )
    }

    pub(super) fn ack(&mut self, command: &Command) {
        self.send(command);
        let seq = read_ack_message(&self.frame()).unwrap();
        assert_eq!(seq, self.seq, "an ack of another command");
    }

    pub(super) fn refused(&mut self, command: &Command) -> PulseError {
        self.send(command);
        match read_ack_message(&self.frame()) {
            Err(ProtocolError::ServerError(error)) => error,
            other => panic!("an error reply expected, got {other:?}"),
        }
    }

    pub(super) fn subscribe(&mut self, mask: SubscriptionMask) {
        self.ack(&Command::Subscribe(mask));
    }

    pub(super) fn event(&mut self) -> SubscriptionEvent {
        match read_command_message(&self.frame()).unwrap() {
            (u32::MAX, Command::SubscribeEvent(event)) => event,
            other => panic!("an event expected, got {other:?}"),
        }
    }

    pub(super) fn nothing_for(&mut self, quiet: Duration) {
        self.stream.set_read_timeout(Some(quiet)).unwrap();
        let mut byte = [0];
        match self.stream.read(&mut byte) {
            Ok(0) => panic!("the fake closed the connection during a quiet wait"),
            Ok(_) => panic!("a frame arrived during a quiet wait"),
            Err(error) => assert!(
                matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
                "{error}"
            ),
        }
        self.stream.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();
    }

    pub(super) fn closed(&mut self) -> bool {
        let mut byte = [0];
        matches!(self.stream.read(&mut byte), Ok(0))
    }
}
