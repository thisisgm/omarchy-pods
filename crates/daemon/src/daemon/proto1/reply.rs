//! The lines the daemon writes in protocol 1, each ended by a newline, members in the documented order.

use pods_core::command::Outcome;
use pods_core::state::State;
use pods_core::status::to_status_object;
use serde_json::{Value, json};

use super::request::RequestError;
use crate::protocol::VERSION;

/// The code of a request in a version this daemon does not speak; every other code is a `CommandError` code.
const UNSUPPORTED_VERSION: &str = "unsupported_version";

/// The sentence a person reads for `UNSUPPORTED_VERSION`.
const UNSUPPORTED_VERSION_MESSAGE: &str = "This daemon speaks protocol version 1 only.";

/// The reply to `get` and `subscribe`.
pub(super) fn state(id: u64, state: &State) -> String {
    format!(
        "{{\"id\":{id},\"ok\":true,\"state\":{}}}\n",
        to_status_object(state)
    )
}

/// The reply to `set` and `do`.
pub(super) fn outcome(id: u64, outcome: Outcome) -> String {
    let result = match outcome {
        Outcome::Sent => "sent",
        Outcome::Confirmed => "confirmed",
        Outcome::Refused(error) => return refusal(Some(id), RequestError::Command(error)),
    };
    format!("{{\"id\":{id},\"ok\":true,\"result\":\"{result}\"}}\n")
}

/// The reply to a refused request; `None` writes the id as null.
pub(super) fn refusal(id: Option<u64>, error: RequestError) -> String {
    let error = match error {
        RequestError::Command(error) => json!({"code": error.code(), "message": error.message()}),
        RequestError::UnsupportedVersion => json!({
            "code": UNSUPPORTED_VERSION,
            "message": UNSUPPORTED_VERSION_MESSAGE,
            "supported": [VERSION],
        }),
    };
    format!(
        "{{\"id\":{},\"ok\":false,\"error\":{error}}}\n",
        Value::from(id)
    )
}

/// The line a subscriber gets for each change.
pub(super) fn event(state: &State) -> String {
    format!(
        "{{\"event\":\"state\",\"state\":{}}}\n",
        to_status_object(state)
    )
}

#[cfg(test)]
mod tests {
    use pods_core::command::CommandError;

    use super::*;

    #[test]
    fn replies_are_the_documented_lines() {
        let status = to_status_object(&State::default());
        assert_eq!(
            state(7, &State::default()),
            format!("{{\"id\":7,\"ok\":true,\"state\":{status}}}\n")
        );
        assert_eq!(
            outcome(9, Outcome::Sent),
            "{\"id\":9,\"ok\":true,\"result\":\"sent\"}\n"
        );
        assert_eq!(
            outcome(9, Outcome::Confirmed),
            "{\"id\":9,\"ok\":true,\"result\":\"confirmed\"}\n"
        );
        assert_eq!(
            outcome(9, Outcome::Refused(CommandError::NotConnected)),
            "{\"id\":9,\"ok\":false,\"error\":{\"code\":\"not_connected\",\"message\":\"The AirPods are not connected.\"}}\n"
        );
        assert_eq!(
            refusal(None, RequestError::Command(CommandError::BadRequest)),
            "{\"id\":null,\"ok\":false,\"error\":{\"code\":\"bad_request\",\"message\":\"The daemon could not read the request.\"}}\n"
        );
        assert_eq!(
            refusal(Some(3), RequestError::UnsupportedVersion),
            "{\"id\":3,\"ok\":false,\"error\":{\"code\":\"unsupported_version\",\"message\":\"This daemon speaks protocol version 1 only.\",\"supported\":[1]}}\n"
        );
        assert_eq!(
            event(&State::default()),
            format!("{{\"event\":\"state\",\"state\":{status}}}\n")
        );
    }

    #[test]
    fn the_protocol_document_lists_every_code_with_its_message() {
        let document = include_str!("../../../../../docs/protocol-1.md");
        for error in CommandError::ALL {
            let row = format!("| `{}` | {} |", error.code(), error.message());
            assert!(document.contains(&row), "no row {row}");
        }
        let row = format!("| `{UNSUPPORTED_VERSION}` | {UNSUPPORTED_VERSION_MESSAGE} |");
        assert!(document.contains(&row), "no row {row}");
    }
}
