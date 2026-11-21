//! The one error type every parser returns.

/// Why a packet was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Fewer bytes than the shortest valid packet.
    #[error("packet too short: need {need} bytes, got {got}")]
    TooShort { need: usize, got: usize },
    /// The packet does not start with the header this parser reads.
    #[error("packet does not start with the expected header")]
    BadHeader,
    /// A byte holds a value the protocol does not define for that field.
    #[error("unknown {field} value {value:#04x}")]
    UnknownValue { field: &'static str, value: u8 },
    /// The length disagrees with the length the packet itself declares.
    #[error("packet is {got} bytes, its count says {expected}")]
    WrongLength { expected: usize, got: usize },
}

/// Checks the minimum length, then returns the bytes after `header`.
pub(crate) fn strip_header<'a>(
    bytes: &'a [u8],
    header: &[u8],
    need: usize,
) -> Result<&'a [u8], ParseError> {
    if bytes.len() < need {
        return Err(ParseError::TooShort {
            need,
            got: bytes.len(),
        });
    }
    bytes.strip_prefix(header).ok_or(ParseError::BadHeader)
}
