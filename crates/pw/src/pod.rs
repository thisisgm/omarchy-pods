//! The two parameter messages the node sends, written as SPA pods by safe code.

const _: () = assert!(
    cfg!(target_endian = "little"),
    "a pod is in the host's byte order, and these are written little endian"
);

// The ids below are from spa/utils/type.h, spa/param/param.h, spa/param/format.h, spa/param/audio/raw.h and spa/param/latency.h.
const TYPE_ID: u32 = 3;
const TYPE_INT: u32 = 4;
const TYPE_LONG: u32 = 5;
const TYPE_FLOAT: u32 = 6;
const TYPE_ARRAY: u32 = 13;
const TYPE_OBJECT: u32 = 15;
const OBJECT_FORMAT: u32 = 0x4_0003;
const OBJECT_PROCESS_LATENCY: u32 = 0x4_000c;
const PARAM_ENUM_FORMAT: u32 = 3;
const PARAM_PROCESS_LATENCY: u32 = 16;
const FORMAT_MEDIA_TYPE: u32 = 1;
const FORMAT_MEDIA_SUBTYPE: u32 = 2;
const FORMAT_AUDIO_FORMAT: u32 = 0x1_0001;
const FORMAT_AUDIO_RATE: u32 = 0x1_0003;
const FORMAT_AUDIO_CHANNELS: u32 = 0x1_0004;
const FORMAT_AUDIO_POSITION: u32 = 0x1_0005;
const MEDIA_TYPE_AUDIO: u32 = 1;
const MEDIA_SUBTYPE_RAW: u32 = 1;
const AUDIO_FORMAT_F32_LE: u32 = 0x11b;
const AUDIO_CHANNEL_MONO: u32 = 2;
const PROCESS_LATENCY_QUANTUM: u32 = 1;
const PROCESS_LATENCY_RATE: u32 = 2;
const PROCESS_LATENCY_NS: u32 = 3;

const WORD: u32 = 4;
/// The bytes of one mono channel list: the array's element size and type, then one id.
const ONE_ID_ARRAY: u32 = 3 * WORD;
/// The words of the format object after its size and type: the object's own two, five one word properties and the channel list.
const FORMAT_WORDS: u32 = 2 + 5 * 6 + 8;
/// The words of the latency object after its size and type, with the two of the nanoseconds.
const LATENCY_WORDS: u32 = 2 + 2 * 6 + 4 + 2;

/// A property whose value is one word: key, flags, the value's size and type, the value and its padding.
const fn property(key: u32, kind: u32, value: u32) -> [u32; 6] {
    [key, 0, WORD, kind, value, 0]
}

fn bytes(words: &[&[u32]]) -> Vec<u8> {
    words
        .iter()
        .flat_map(|part| part.iter())
        .flat_map(|word| word.to_le_bytes())
        .collect()
}

/// `SPA_PARAM_EnumFormat`: raw audio, F32, one channel, MONO, at `rate`.
#[must_use]
pub fn format(rate: u32) -> Vec<u8> {
    bytes(&[
        &[
            FORMAT_WORDS * WORD,
            TYPE_OBJECT,
            OBJECT_FORMAT,
            PARAM_ENUM_FORMAT,
        ],
        &property(FORMAT_MEDIA_TYPE, TYPE_ID, MEDIA_TYPE_AUDIO),
        &property(FORMAT_MEDIA_SUBTYPE, TYPE_ID, MEDIA_SUBTYPE_RAW),
        &property(FORMAT_AUDIO_FORMAT, TYPE_ID, AUDIO_FORMAT_F32_LE),
        &property(FORMAT_AUDIO_RATE, TYPE_INT, rate),
        &property(FORMAT_AUDIO_CHANNELS, TYPE_INT, 1),
        &[FORMAT_AUDIO_POSITION, 0, ONE_ID_ARRAY, TYPE_ARRAY],
        &[WORD, TYPE_ID, AUDIO_CHANNEL_MONO, 0],
    ])
}

/// `SPA_PARAM_ProcessLatency`: `ns` nanoseconds, no quantum share and no frames.
#[must_use]
pub fn process_latency(ns: u64) -> Vec<u8> {
    let mut message = bytes(&[
        &[
            LATENCY_WORDS * WORD,
            TYPE_OBJECT,
            OBJECT_PROCESS_LATENCY,
            PARAM_PROCESS_LATENCY,
        ],
        &property(PROCESS_LATENCY_QUANTUM, TYPE_FLOAT, 0.0_f32.to_bits()),
        &property(PROCESS_LATENCY_RATE, TYPE_INT, 0),
        &[PROCESS_LATENCY_NS, 0, 2 * WORD, TYPE_LONG],
    ]);
    message.extend_from_slice(&ns.to_le_bytes());
    message
}
