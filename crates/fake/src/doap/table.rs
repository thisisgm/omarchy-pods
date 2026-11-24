//! How the pods' table answers the requests of a walk: the declarations, the types, the codec list, and a refusal for any other read request or write request; no other packet is answered.

use std::ops::RangeInclusive;

use pods_samples::doap::{
    self as sample, ATTRIBUTE_NOT_FOUND, CONFIGURATION, DECLARATION, FIND_INFORMATION_REQUEST,
    Handles, READ_BY_TYPE_REQUEST, READ_NOT_PERMITTED, READ_REQUEST, WRITE_NOT_PERMITTED,
    WRITE_REQUEST, host,
};

/// The declarations of 21 bytes a reply holds on an L2CAP channel of the default MTU of 48, behind the two bytes it opens with.
const DECLARATIONS_A_REPLY: usize = 2;

#[derive(Debug, Clone, Copy)]
enum Kind {
    Short(u16),
    Long([u8; 16]),
}

/// The answer to a request of the host, as `08 01 00 ff ff 03 28` asks for declarations; a table of `None` holds no attribute, and what is no request has no answer.
pub(super) fn answer(handles: Option<&Handles>, pdu: &[u8]) -> Option<Vec<u8>> {
    match *pdu {
        [READ_BY_TYPE_REQUEST, s0, s1, e0, e1, k0, k1] => {
            let asked = u16::from_le_bytes([s0, s1])..=u16::from_le_bytes([e0, e1]);
            let declarations = u16::from_le_bytes([k0, k1]) == DECLARATION;
            let handles = handles.filter(|_| declarations);
            Some(declared(handles, &asked))
        }
        [FIND_INFORMATION_REQUEST, s0, s1, e0, e1] => {
            let asked = u16::from_le_bytes([s0, s1])..=u16::from_le_bytes([e0, e1]);
            Some(types(handles, &asked))
        }
        [READ_REQUEST, h0, h1] => {
            let handle = u16::from_le_bytes([h0, h1]);
            let codecs =
                handles.is_some_and(|handles| *pdu == host::read(handles.supported_codecs));
            // corner: assumed until measured on hardware, the code a refused read or write carries
            let refusal = sample::error(READ_REQUEST, handle, READ_NOT_PERMITTED);
            Some(if codecs {
                sample::CODECS.to_vec()
            } else {
                refusal.to_vec()
            })
        }
        [WRITE_REQUEST, h0, h1, ..] => {
            let handle = u16::from_le_bytes([h0, h1]);
            Some(sample::error(WRITE_REQUEST, handle, WRITE_NOT_PERMITTED).to_vec())
        }
        _ => None,
    }
}

fn not_found(request: u8, asked: &RangeInclusive<u16>) -> Vec<u8> {
    sample::error(request, *asked.start(), ATTRIBUTE_NOT_FOUND).to_vec()
}

fn declared(handles: Option<&Handles>, asked: &RangeInclusive<u16>) -> Vec<u8> {
    let table = handles.map(Handles::declared);
    let listed: Vec<_> = table
        .iter()
        .flatten()
        .filter(|declared| asked.contains(&declared.at))
        .take(DECLARATIONS_A_REPLY)
        .copied()
        .collect();
    if listed.is_empty() {
        return not_found(READ_BY_TYPE_REQUEST, asked);
    }
    sample::declarations(&listed)
}

/// Every attribute of the table with its type, in rising order; the service declaration is not served.
fn attributes(handles: &Handles) -> Vec<(u16, Kind)> {
    let mut attributes = Vec::new();
    for declared in handles.declared() {
        attributes.push((declared.at, Kind::Short(DECLARATION)));
        attributes.push((declared.value, Kind::Long(declared.uuid)));
    }
    let descriptors = handles.cccds.map(|at| (at, Kind::Short(CONFIGURATION)));
    attributes.extend(descriptors);
    attributes.sort_by_key(|(at, _)| *at);
    attributes
}

/// One listing holds types of one width, so it ends where the width changes; no run of this table is longer than a reply.
fn types(handles: Option<&Handles>, asked: &RangeInclusive<u16>) -> Vec<u8> {
    let attributes = handles.map_or_default(attributes);
    let mut listed = attributes
        .into_iter()
        .filter(|(at, _)| asked.contains(at))
        .peekable();
    match listed.peek() {
        None => not_found(FIND_INFORMATION_REQUEST, asked),
        Some((_, Kind::Short(_))) => {
            let short = listed.map_while(|(at, kind)| match kind {
                Kind::Short(kind) => Some((at, kind)),
                Kind::Long(_) => None,
            });
            sample::short_types(&short.collect::<Vec<_>>())
        }
        Some((_, Kind::Long(_))) => {
            let long = listed.map_while(|(at, kind)| match kind {
                Kind::Long(uuid) => Some((at, uuid)),
                Kind::Short(_) => None,
            });
            sample::long_types(&long.collect::<Vec<_>>())
        }
    }
}
