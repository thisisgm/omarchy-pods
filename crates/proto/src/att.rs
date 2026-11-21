//! The attribute protocol (ATT) messages a host needs to read a peer's attribute table, with no I/O.

use crate::error::{ParseError, Parser};

// Layouts and numbers are those of the Bluetooth Core Specification 6.1, Vol 3, Part F (ATT) and Part G (GATT).

/// `ATT_ERROR_RSP` (Vol 3, Part F, 3.4.1.1).
const ERROR_RESPONSE: u8 = 0x01;
/// `ATT_FIND_INFORMATION_REQ` (Vol 3, Part F, 3.4.3.1).
const FIND_INFORMATION_REQUEST: u8 = 0x04;
/// `ATT_FIND_INFORMATION_RSP` (Vol 3, Part F, 3.4.3.2).
const FIND_INFORMATION_RESPONSE: u8 = 0x05;
/// `ATT_READ_BY_TYPE_REQ` (Vol 3, Part F, 3.4.4.1).
const READ_BY_TYPE_REQUEST: u8 = 0x08;
/// `ATT_READ_BY_TYPE_RSP` (Vol 3, Part F, 3.4.4.2).
const READ_BY_TYPE_RESPONSE: u8 = 0x09;
/// `ATT_HANDLE_VALUE_NTF` (Vol 3, Part F, 3.4.7.1).
pub(crate) const HANDLE_VALUE_NOTIFICATION: u8 = 0x1b;
/// `ATT_HANDLE_VALUE_IND` (Vol 3, Part F, 3.4.7.2).
const HANDLE_VALUE_INDICATION: u8 = 0x1d;
/// `ATT_HANDLE_VALUE_CFM` (Vol 3, Part F, 3.4.7.3).
const HANDLE_VALUE_CONFIRMATION: u8 = 0x1e;

/// Every opcode the opcode summary names a request (Vol 3, Part F, 3.4.8, Table 3.43).
const REQUEST_OPCODES: [u8; 12] = [
    0x02, // ATT_EXCHANGE_MTU_REQ
    FIND_INFORMATION_REQUEST,
    0x06, // ATT_FIND_BY_TYPE_VALUE_REQ
    READ_BY_TYPE_REQUEST,
    0x0a, // ATT_READ_REQ
    0x0c, // ATT_READ_BLOB_REQ
    0x0e, // ATT_READ_MULTIPLE_REQ
    0x10, // ATT_READ_BY_GROUP_TYPE_REQ
    0x12, // ATT_WRITE_REQ
    0x16, // ATT_PREPARE_WRITE_REQ
    0x18, // ATT_EXECUTE_WRITE_REQ
    0x20, // ATT_READ_MULTIPLE_VARIABLE_REQ
];

/// Format of a Find Information reply that pairs handles with 16 bit identifiers (Vol 3, Part F, 3.4.3.2, Table 3.9).
const FORMAT_SHORT: u8 = 0x01;
/// Format of a Find Information reply that pairs handles with 128 bit identifiers (Vol 3, Part F, 3.4.3.2, Table 3.9).
const FORMAT_LONG: u8 = 0x02;
/// The opcode alone, the shortest PDU.
const OPCODE_LEN: usize = 1;
/// An attribute handle is a little endian u16 (Vol 3, Part F, 3.3.1).
const HANDLE_LEN: usize = 2;
/// A 16 bit Bluetooth identifier.
const SHORT_UUID_LEN: usize = 2;
/// A 128 bit identifier.
const LONG_UUID_LEN: usize = 16;
/// The opcode and the format or entry length behind it, the shortest list reply.
const LIST_MIN_LEN: usize = 2;
/// An entry of a Read By Type reply is a handle and a value, so it is never shorter than a handle (Vol 3, Part F, 3.4.4.2).
const ENTRY_MIN_LEN: u8 = 2;
/// Opcode, request opcode, handle and error code: an error response is always this long (Vol 3, Part F, 3.4.1.1).
const ERROR_LEN: usize = 5;
/// The request opcode, the handle and the error code behind the opcode of an error response.
const ERROR_BODY_LEN: usize = 4;
/// Opcode and handle, the shortest notification or indication.
const HANDLE_VALUE_MIN_LEN: usize = 3;
/// The property byte and the value handle that open a characteristic declaration (Vol 3, Part G, 3.3.1, Table 3.4).
const DECLARATION_FIXED_LEN: usize = 3;

/// The attribute type of a characteristic declaration (Vol 3, Part G, 3.3.1).
pub const CHARACTERISTIC_DECLARATION: u16 = 0x2803;
/// The property bit of a value that takes a write with no response (Vol 3, Part G, 3.3.1.1).
pub const PROPERTY_WRITE_WITHOUT_RESPONSE: u8 = 0x04;
/// The property bit of a value the peer may notify (Vol 3, Part G, 3.3.1.1).
pub const PROPERTY_NOTIFY: u8 = 0x10;
/// The error code for a handle range that holds no matching attribute, which ends a walk of the table (Vol 3, Part F, 3.4.1.1).
pub const ATTRIBUTE_NOT_FOUND: u8 = 0x0a;
/// The error code a server sends for a request it does not support, with handle 0x0000 (Vol 3, Part F, 3.3).
pub const REQUEST_NOT_SUPPORTED: u8 = 0x06;
/// The answer every indication must get before the peer sends another (Vol 3, Part F, 3.4.7.2).
pub const CONFIRMATION: [u8; 1] = [HANDLE_VALUE_CONFIRMATION];

/// Asks for the handle and type of every attribute from `start` to `end`; 0x0001 to 0xffff is the whole table.
// Sample output for (0x0001, 0xffff): 04 01 00 ff ff.
#[must_use]
pub fn find_information_request(start: u16, end: u16) -> [u8; 5] {
    let [start_0, start_1] = start.to_le_bytes();
    let [end_0, end_1] = end.to_le_bytes();
    [FIND_INFORMATION_REQUEST, start_0, start_1, end_0, end_1]
}

/// Asks for the value of every attribute of type `kind` from `start` to `end`.
// Sample output for (0x0001, 0xffff, 0x2803): 08 01 00 ff ff 03 28.
#[must_use]
pub fn read_by_type_request(start: u16, end: u16, kind: u16) -> [u8; 7] {
    let [start_0, start_1] = start.to_le_bytes();
    let [end_0, end_1] = end.to_le_bytes();
    let [kind_0, kind_1] = kind.to_le_bytes();
    [
        READ_BY_TYPE_REQUEST,
        start_0,
        start_1,
        end_0,
        end_1,
        kind_0,
        kind_1,
    ]
}

/// Refuses the peer's request `request_opcode` on `handle` with `code`.
// Sample output for (0x02, 0x0000, 0x06): 01 02 00 00 06.
#[must_use]
pub fn error_response(request_opcode: u8, handle: u16, code: u8) -> [u8; 5] {
    let [handle_0, handle_1] = handle.to_le_bytes();
    [ERROR_RESPONSE, request_opcode, handle_0, handle_1, code]
}

/// An attribute type or a characteristic identifier, in the size the peer sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uuid<'a> {
    /// A 16 bit Bluetooth identifier.
    Short(u16),
    /// A 128 bit identifier in wire order, least significant octet first.
    Long(&'a [u8; LONG_UUID_LEN]),
}

impl<'a> Uuid<'a> {
    // 03 28 is the 16 bit identifier 0x2803; sixteen octets are a 128 bit identifier; any other length is neither.
    fn read(bytes: &'a [u8]) -> Option<Self> {
        if let Ok(short) = <&[u8; SHORT_UUID_LEN]>::try_from(bytes) {
            Some(Self::Short(u16::from_le_bytes(*short)))
        } else if let Ok(long) = <&[u8; LONG_UUID_LEN]>::try_from(bytes) {
            Some(Self::Long(long))
        } else {
            None
        }
    }
}

/// One attribute of the peer's table: its handle and its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandleInfo<'a> {
    pub handle: u16,
    pub uuid: Uuid<'a>,
}

/// What a characteristic declaration says about the value that follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Characteristic<'a> {
    /// The handle of the declaration itself.
    pub declaration: u16,
    /// The property bits, of which `PROPERTY_NOTIFY` and `PROPERTY_WRITE_WITHOUT_RESPONSE` are two.
    pub properties: u8,
    /// The handle the value is read, written or notified on.
    pub value_handle: u16,
    pub uuid: Uuid<'a>,
}

/// Reads one entry of a Read By Type reply for `CHARACTERISTIC_DECLARATION`; `None` is an entry of any other length.
// 02 00 14 03 00 2a 2b: declaration handle 0x0002, properties 0x14, value handle 0x0003, then a 2 or 16 byte identifier.
#[must_use]
pub fn characteristic(entry: &[u8]) -> Option<Characteristic<'_>> {
    let (declaration, value) = entry.split_first_chunk::<HANDLE_LEN>()?;
    let ([properties, value_handle @ ..], uuid) =
        value.split_first_chunk::<DECLARATION_FIXED_LEN>()?;
    Some(Characteristic {
        declaration: u16::from_le_bytes(*declaration),
        properties: *properties,
        value_handle: u16::from_le_bytes(*value_handle),
        uuid: Uuid::read(uuid)?,
    })
}

/// One attribute protocol PDU, as a host that reads the table and listens for values sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttPdu<'a> {
    /// The reply to `find_information_request`.
    Information(Information<'a>),
    /// The reply to `read_by_type_request`.
    ByType(ByType<'a>),
    /// The peer refused the request with opcode `request`; `ATTRIBUTE_NOT_FOUND` is the ordinary end of a walk.
    Error { request: u8, handle: u16, code: u8 },
    /// A value the peer sent unasked, which gets no answer.
    Notification { handle: u16, value: &'a [u8] },
    /// A value the peer sent unasked, which must be answered with `CONFIRMATION`.
    Indication { handle: u16, value: &'a [u8] },
    /// The peer asks this host something and waits for a reply, which `error_response` builds.
    Request { opcode: u8 },
    /// Any other opcode, kept whole so the caller can log or count it.
    Other { opcode: u8, body: &'a [u8] },
}

impl<'a> AttPdu<'a> {
    /// Reads one SDU of an ATT channel.
    // 1b 21 43 b8 00: the opcode (0x1b, a notification), then what that opcode defines, here handle 0x4321 and a two byte value.
    pub fn parse(pdu: &'a [u8]) -> Result<Self, ParseError> {
        let Some((&opcode, body)) = pdu.split_first() else {
            return Err(ParseError::TooShort {
                parser: Parser::Att,
                need: OPCODE_LEN,
                got: pdu.len(),
            });
        };
        match opcode {
            FIND_INFORMATION_RESPONSE => Information::parse(body, pdu.len()).map(Self::Information),
            READ_BY_TYPE_RESPONSE => ByType::parse(body, pdu.len()).map(Self::ByType),
            ERROR_RESPONSE => Self::parse_error(body, pdu.len()),
            HANDLE_VALUE_NOTIFICATION => handle_value(Parser::AttNotification, body, pdu.len())
                .map(|(handle, value)| Self::Notification { handle, value }),
            HANDLE_VALUE_INDICATION => handle_value(Parser::AttIndication, body, pdu.len())
                .map(|(handle, value)| Self::Indication { handle, value }),
            opcode if REQUEST_OPCODES.contains(&opcode) => Ok(Self::Request { opcode }),
            opcode => Ok(Self::Other { opcode, body }),
        }
    }

    // 04 01 00 0a: the refused request's opcode 0x04, handle 0x0001, error code 0x0a, and nothing after.
    fn parse_error(body: &[u8], pdu_len: usize) -> Result<Self, ParseError> {
        let Ok([request, handle @ .., code]) = <&[u8; ERROR_BODY_LEN]>::try_from(body) else {
            return Err(ParseError::WrongLength {
                parser: Parser::AttError,
                expected: ERROR_LEN,
                got: pdu_len,
            });
        };
        Ok(Self::Error {
            request: *request,
            handle: u16::from_le_bytes(*handle),
            code: *code,
        })
    }
}

// 21 43 b8 00: handle 0x4321 little endian, then the value, which may be empty.
fn handle_value(parser: Parser, body: &[u8], pdu_len: usize) -> Result<(u16, &[u8]), ParseError> {
    let Some((handle, value)) = body.split_first_chunk::<HANDLE_LEN>() else {
        return Err(ParseError::TooShort {
            parser,
            need: HANDLE_VALUE_MIN_LEN,
            got: pdu_len,
        });
    };
    Ok((u16::from_le_bytes(*handle), value))
}

/// The size of the identifiers in one Find Information reply, which its format byte sets for every pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UuidSize {
    Short,
    Long,
}

/// Walks the handle and type pairs of one Find Information reply, which holds at least one, and stops at the first one that is cut short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Information<'a> {
    size: UuidSize,
    rest: &'a [u8],
}

impl<'a> Information<'a> {
    // 01 01 00 00 28 02 00 03 28: format 1, then handle 0x0001 of type 0x2800 and handle 0x0002 of type 0x2803.
    fn parse(body: &'a [u8], pdu_len: usize) -> Result<Self, ParseError> {
        let Some((&format, rest)) = body.split_first() else {
            return Err(ParseError::TooShort {
                parser: Parser::AttFindInformation,
                need: LIST_MIN_LEN,
                got: pdu_len,
            });
        };
        let size = match format {
            FORMAT_SHORT => UuidSize::Short,
            FORMAT_LONG => UuidSize::Long,
            value => {
                return Err(ParseError::UnknownValue {
                    parser: Parser::AttFindInformation,
                    field: "format",
                    value,
                });
            }
        };
        let pairs = Self { size, rest };
        // The information data holds at least one pair (Vol 3, Part F, 3.4.3.2, Table 3.8), and a walk has no next handle without one.
        if pairs.clone().next().is_none() {
            return Err(ParseError::EmptyList {
                parser: Parser::AttFindInformation,
                list_len: rest.len(),
            });
        }
        Ok(pairs)
    }

    /// The bytes not yielded yet; once `next` returns `None`, the bytes that hold no whole pair.
    #[must_use]
    pub fn trailing(&self) -> usize {
        self.rest.len()
    }
}

impl<'a> Iterator for Information<'a> {
    type Item = HandleInfo<'a>;

    // 02 00 03 28: handle 0x0002 little endian, then an identifier of the size the format byte set.
    fn next(&mut self) -> Option<HandleInfo<'a>> {
        let (handle, after_handle) = self.rest.split_first_chunk::<HANDLE_LEN>()?;
        let uuid_len = match self.size {
            UuidSize::Short => SHORT_UUID_LEN,
            UuidSize::Long => LONG_UUID_LEN,
        };
        let (uuid, rest) = after_handle.split_at_checked(uuid_len)?;
        let uuid = Uuid::read(uuid)?;
        self.rest = rest;
        Some(HandleInfo {
            handle: u16::from_le_bytes(*handle),
            uuid,
        })
    }
}

/// Walks the entries of one Read By Type reply, at least one and each a handle and a value, and stops at the first one that is cut short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByType<'a> {
    entry_len: usize,
    rest: &'a [u8],
}

impl<'a> ByType<'a> {
    // 07 02 00 14 03 00 2a 2b: every entry is 7 bytes, then one entry.
    fn parse(body: &'a [u8], pdu_len: usize) -> Result<Self, ParseError> {
        let Some((&entry_len, rest)) = body.split_first() else {
            return Err(ParseError::TooShort {
                parser: Parser::AttReadByType,
                need: LIST_MIN_LEN,
                got: pdu_len,
            });
        };
        // An entry length of zero would make `next` yield empty entries without end.
        if entry_len < ENTRY_MIN_LEN {
            return Err(ParseError::UnknownValue {
                parser: Parser::AttReadByType,
                field: "entry length",
                value: entry_len,
            });
        }
        let entries = Self {
            entry_len: usize::from(entry_len),
            rest,
        };
        // The attribute data list holds at least one entry (Vol 3, Part F, 3.4.4.2, Table 3.16), and a walk has no next handle without one.
        if entries.clone().next().is_none() {
            return Err(ParseError::EmptyList {
                parser: Parser::AttReadByType,
                list_len: rest.len(),
            });
        }
        Ok(entries)
    }

    /// The bytes not yielded yet; once `next` returns `None`, the bytes that hold no whole entry.
    #[must_use]
    pub fn trailing(&self) -> usize {
        self.rest.len()
    }
}

impl<'a> Iterator for ByType<'a> {
    type Item = &'a [u8];

    // 02 00 14 03 00 2a 2b with an entry length of 7: one whole entry, handle first.
    fn next(&mut self) -> Option<&'a [u8]> {
        let (entry, rest) = self.rest.split_at_checked(self.entry_len)?;
        self.rest = rest;
        Some(entry)
    }
}

#[cfg(test)]
mod tests;
