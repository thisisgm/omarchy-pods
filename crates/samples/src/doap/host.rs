//! What a host writes to the dictation service.

use super::{
    CONFIRMATION, DECLARATION, ERROR_RESPONSE, FIND_INFORMATION_REQUEST, Handles, NOTIFY,
    ONE_STREAM, OPUS, READ_BY_TYPE_REQUEST, READ_REQUEST, REQUEST_NOT_SUPPORTED, STREAM,
    WRITE_COMMAND, WRITE_REQUEST,
};

/// The highest attribute handle, where a walk of the whole table ends.
const LAST_HANDLE: [u8; 2] = [0xff, 0xff];

/// The whole of the packet a host confirms an indication with.
pub const CONFIRMED: [u8; 1] = [CONFIRMATION];

/// `08 01 00 ff ff 03 28`: every characteristic declaration from `from` on.
#[must_use]
pub fn declarations_from(from: u16) -> [u8; 7] {
    let [low, high] = from.to_le_bytes();
    let [last_low, last_high] = LAST_HANDLE;
    let [kind_low, kind_high] = DECLARATION.to_le_bytes();
    let request = READ_BY_TYPE_REQUEST;
    [request, low, high, last_low, last_high, kind_low, kind_high]
}

/// `04 08 00 11 00`: the type of every attribute from `from` to `to`.
#[must_use]
pub fn types_between(from: u16, to: u16) -> [u8; 5] {
    let [low, high] = from.to_le_bytes();
    let [to_low, to_high] = to.to_le_bytes();
    [FIND_INFORMATION_REQUEST, low, high, to_low, to_high]
}

/// `12 08 00 01 00`: turns on the notifications of the characteristic the descriptor belongs to.
#[must_use]
pub fn subscribe(descriptor: u16) -> [u8; 5] {
    let [low, high] = descriptor.to_le_bytes();
    let [notify_low, notify_high] = NOTIFY;
    [WRITE_REQUEST, low, high, notify_low, notify_high]
}

/// `0a 03 00`.
#[must_use]
pub fn read(handle: u16) -> [u8; 3] {
    let [low, high] = handle.to_le_bytes();
    [READ_REQUEST, low, high]
}

/// `52 05 00 01 01 00 07`: one stream, its identifier, Opus.
#[must_use]
pub fn select_codec(handles: &Handles) -> [u8; 7] {
    let [low, high] = handles.select_codec.to_le_bytes();
    let [stream_low, stream_high] = STREAM;
    let command = WRITE_COMMAND;
    [
        command,
        low,
        high,
        ONE_STREAM,
        stream_low,
        stream_high,
        OPUS,
    ]
}

/// `52 07 00 01 00`.
#[must_use]
pub fn start(handles: &Handles) -> [u8; 5] {
    let [low, high] = handles.start.to_le_bytes();
    let [stream_low, stream_high] = STREAM;
    [WRITE_COMMAND, low, high, stream_low, stream_high]
}

/// `52 0a 00 01 00 00`: the stream and the reason.
#[must_use]
pub fn stop(handles: &Handles, reason: u8) -> [u8; 6] {
    let [low, high] = handles.stop.to_le_bytes();
    let [stream_low, stream_high] = STREAM;
    let command = WRITE_COMMAND;
    [command, low, high, stream_low, stream_high, reason]
}

/// `01 02 00 00 06`: a request of the pods' own, refused as not supported.
#[must_use]
pub fn refusal(request: u8) -> [u8; 5] {
    [ERROR_RESPONSE, request, 0x00, 0x00, REQUEST_NOT_SUPPORTED]
}

#[cfg(test)]
mod tests {
    use super::super::{REASON_HOST, REASON_UNASKED};
    use super::*;

    #[test]
    fn the_writes_are_the_bytes_seen_at_the_observed_handles() {
        let table = Handles::OBSERVED;
        assert_eq!(
            declarations_from(0x0001),
            [0x08, 0x01, 0x00, 0xff, 0xff, 0x03, 0x28]
        );
        assert_eq!(
            types_between(0x0008, 0x0011),
            [0x04, 0x08, 0x00, 0x11, 0x00]
        );
        assert_eq!(subscribe(0x0008), [0x12, 0x08, 0x00, 0x01, 0x00]);
        assert_eq!(read(table.supported_codecs), [0x0a, 0x03, 0x00]);
        assert_eq!(
            select_codec(&table),
            [0x52, 0x05, 0x00, 0x01, 0x01, 0x00, 0x07]
        );
        assert_eq!(start(&table), [0x52, 0x07, 0x00, 0x01, 0x00]);
        assert_eq!(
            stop(&table, REASON_HOST),
            [0x52, 0x0a, 0x00, 0x01, 0x00, 0x00]
        );
        assert_eq!(stop(&table, REASON_UNASKED)[5], 0x06);
        assert_eq!(refusal(0x02), [0x01, 0x02, 0x00, 0x00, 0x06]);
        assert_eq!(CONFIRMED, [0x1e]);
    }
}
