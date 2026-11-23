//! Reads back the two parameter messages a source node sends, sharing no line with the code that writes them.

/// The properties of a format message, and of a process latency message; an object with another count is neither.
const FORMAT_PROPERTIES: usize = 6;
const LATENCY_PROPERTIES: usize = 3;
/// A pod pads every value to this many bytes.
const ALIGN: usize = 8;
/// The bytes of one id, and of a channel list that holds one: the element size, the element type and the id.
const ID_BYTES: u32 = 4;
const ONE_ID_LIST: usize = 12;

// The ids are from spa/utils/type.h, spa/param/param.h, spa/param/format.h, spa/param/audio/raw.h and spa/param/latency.h.
const ID: u32 = 3;
const INT: u32 = 4;
const LONG: u32 = 5;
const FLOAT: u32 = 6;
const ARRAY: u32 = 13;
const OBJECT: u32 = 15;
const FORMAT: (u32, u32) = (0x4_0003, 3);
const PROCESS_LATENCY: (u32, u32) = (0x4_000c, 16);
const MEDIA_TYPE: (u32, u32) = (1, ID);
const MEDIA_SUBTYPE: (u32, u32) = (2, ID);
const AUDIO_FORMAT: (u32, u32) = (0x1_0001, ID);
const AUDIO_RATE: (u32, u32) = (0x1_0003, INT);
const AUDIO_CHANNELS: (u32, u32) = (0x1_0004, INT);
const AUDIO_POSITION: (u32, u32) = (0x1_0005, ARRAY);
const LATENCY_QUANTUM: (u32, u32) = (1, FLOAT);
const LATENCY_RATE: (u32, u32) = (2, INT);
const LATENCY_NS: (u32, u32) = (3, LONG);
const AUDIO: u32 = 1;
const RAW: u32 = 1;
const F32_LE: u32 = 0x11b;
const MONO: u32 = 2;

/// One property of an object: its key, the type of its value and the value's bytes without padding.
type Property<'a> = ((u32, u32), &'a [u8]);

fn word(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let (word, rest) = bytes.split_first_chunk::<4>()?;
    Some((u32::from_le_bytes(*word), rest))
}

/// The `count` properties of the object `kind` that `bytes` hold from end to end, each with no flag and zero padding, as the node writes them.
// Sample input: `20 00 00 00 0f 00 00 00 | 0c 00 04 00 10 00 00 00 | 02 00 00 00 00 00 00 00 04 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00`: size, Object; object type, param id; then key, flags, value size, value type and the value padded to 8.
fn properties(bytes: &[u8], kind: (u32, u32), count: usize) -> Option<Vec<Property<'_>>> {
    let (size, rest) = word(bytes)?;
    let (pod, rest) = word(rest)?;
    if pod != OBJECT || usize::try_from(size).ok() != Some(rest.len()) {
        return None;
    }
    let (object, rest) = word(rest)?;
    let (param, mut rest) = word(rest)?;
    if (object, param) != kind {
        return None;
    }
    let mut found = Vec::new();
    while !rest.is_empty() {
        let (key, after) = word(rest)?;
        let (flags, after) = word(after)?;
        let (size, after) = word(after)?;
        let (value, after) = word(after)?;
        let size = usize::try_from(size).ok()?;
        let (padded, next) = after.split_at_checked(size.checked_next_multiple_of(ALIGN)?)?;
        let (body, padding) = padded.split_at_checked(size)?;
        if flags != 0 || padding.iter().any(|byte| *byte != 0) {
            return None;
        }
        found.push(((key, value), body));
        rest = next;
    }
    (found.len() == count).then_some(found)
}

fn value<'a, const N: usize>(properties: &[Property<'a>], key: (u32, u32)) -> Option<&'a [u8; N]> {
    let (_, body) = properties.iter().find(|(found, _)| *found == key)?;
    <&[u8; N]>::try_from(*body).ok()
}

fn one_word(properties: &[Property<'_>], key: (u32, u32)) -> Option<u32> {
    value(properties, key).map(|bytes| u32::from_le_bytes(*bytes))
}

/// The rate of a format message that offers raw audio, F32, one channel, MONO and nothing else.
#[must_use]
pub fn read_format(bytes: &[u8]) -> Option<u32> {
    let properties = properties(bytes, FORMAT, FORMAT_PROPERTIES)?;
    // Sample input: `04 00 00 00 03 00 00 00 02 00 00 00`: the element size, the element type Id, one channel position.
    let position = value::<ONE_ID_LIST>(&properties, AUDIO_POSITION)?;
    let mono = [ID_BYTES, ID, MONO].map(u32::to_le_bytes);
    let fixed = [
        (MEDIA_TYPE, AUDIO),
        (MEDIA_SUBTYPE, RAW),
        (AUDIO_FORMAT, F32_LE),
        (AUDIO_CHANNELS, 1),
    ];
    let offered = *position.as_chunks::<4>().0 == mono
        && fixed
            .iter()
            .all(|(key, wanted)| one_word(&properties, *key) == Some(*wanted));
    offered.then(|| one_word(&properties, AUDIO_RATE)).flatten()
}

/// The nanoseconds of a process latency message that states no quantum share and no frames.
#[must_use]
pub fn read_process_latency(bytes: &[u8]) -> Option<u64> {
    let properties = properties(bytes, PROCESS_LATENCY, LATENCY_PROPERTIES)?;
    let only_ns = one_word(&properties, LATENCY_QUANTUM) == Some(0.0_f32.to_bits())
        && one_word(&properties, LATENCY_RATE) == Some(0);
    let ns = value(&properties, LATENCY_NS)?;
    only_ns.then(|| u64::from_le_bytes(*ns))
}
