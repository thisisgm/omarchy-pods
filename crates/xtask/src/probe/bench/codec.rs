//! The playback codec bench: each reference excerpt through SBC-XQ and through AAC as `PipeWire` sets them, each decode scored against the excerpt.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;

use super::super::analyse::report::{Row, table};
use super::cache::{Cache, CacheError};
use super::pins::{EXCERPT_S, LIBFDK_AAC, LIBSBC, Library, Pin, TRACKS, Track};
use super::visqol::{self, Score, ScorerError};
use super::{ToolError, output_of, write_new};

/// The steps that call a C library, run by the interpreter of the cache.
const TOOL: &str = include_str!("tool.py");
/// `A2DP_CODEC_DEFAULT_RATE`, the rate `PipeWire` asks of a sink that offers it, and the rate `ViSQOL` scores music at.
const RATE_HZ: u32 = 48_000;
/// The key of the rows that score an excerpt against itself, which is the most a decode of it can score.
const REFERENCE: &str = "reference";
/// The most characters of a line that is no name and version an error carries.
const LINE_CHARS: usize = 80;

/// One field or parameter a codec library is given, which the tool reads as `name=value`.
#[derive(Debug, Clone, Copy)]
struct Setting(&'static str, u32);

const SBC_FREQ_48000: u32 = 3;
const SBC_BLK_16: u32 = 3;
const SBC_SB_8: u32 = 1;
const SBC_MODE_DUAL_CHANNEL: u32 = 1;
const SBC_AM_LOUDNESS: u32 = 0;
const SBC_LE: u32 = 0;
/// `default_bitpool` for 48 kHz, dual channel and XQ, line 94; a sink that states a lower greatest bitpool gets that one (209).
const SBC_XQ_BITPOOL: u32 = 39;
// PipeWire 1.6.9 spa/plugins/bluez5/a2dp-codec-sbc.c: sbc_xq prefers dual channel (129-130), takes 16 blocks, 8 subbands and loudness (181-200), asks for its bitpool a channel (206-209) and sets the encoder so (404-490).
const SBC_XQ: [Setting; 7] = [
    Setting("frequency", SBC_FREQ_48000),
    Setting("blocks", SBC_BLK_16),
    Setting("subbands", SBC_SB_8),
    Setting("mode", SBC_MODE_DUAL_CHANNEL),
    Setting("allocation", SBC_AM_LOUDNESS),
    Setting("bitpool", SBC_XQ_BITPOOL),
    Setting("endian", SBC_LE),
];

/// The payload of one 3-DH5 packet, the most one packet of the radio carries.
const EDR_3DH5_PAYLOAD_BYTES: u32 = 1021;
/// The L2CAP header that payload begins with.
const L2CAP_HEADER_BYTES: u32 = 4;
/// Assumed, not read from the pods: a media channel whose largest packet fills one 3-DH5 packet.
const MTU_BYTES: u32 = EDR_3DH5_PAYLOAD_BYTES - L2CAP_HEADER_BYTES;
/// The size of `struct rtp_header`, rtp.h:24-37.
const RTP_HEADER_BYTES: u32 = 12;
const BITS_PER_BYTE: u32 = 8;
const AAC_FRAME_SAMPLES: u32 = 1024;
/// a2dp-codec-aac.c:382: every frame must fit one packet, 376 875 bit/s for the packet assumed.
const AAC_PEAK_BIT_RATE: u32 =
    (MTU_BYTES - RTP_HEADER_BYTES) * BITS_PER_BYTE * RATE_HZ / AAC_FRAME_SAMPLES;
/// `DEFAULT_AAC_BITRATE`, which line 383 gives for a sink that states that much or nothing, as is assumed; the encoder does not read it in a variable rate mode.
const AAC_BIT_RATE: u32 = 320_000;
const AACENC_AOT: &str = "0x0100";
const AACENC_BITRATE: &str = "0x0101";
const AACENC_BITRATEMODE: &str = "0x0102";
const AACENC_SAMPLERATE: &str = "0x0103";
const AACENC_CHANNELMODE: &str = "0x0106";
const AACENC_AFTERBURNER: &str = "0x0200";
const AACENC_PEAK_BITRATE: &str = "0x0207";
const AACENC_TRANSMUX: &str = "0x0300";
const AACENC_HEADER_PERIOD: &str = "0x0301";
const AACENC_SIGNALING_MODE: &str = "0x0302";
const AACENC_AUDIOMUXVER: &str = "0x0304";
const AOT_MP2_AAC_LC: u32 = 129;
const MODE_2: u32 = 2;
const TT_MP4_LATM_MCP1: u32 = 6;
/// The fifth and highest of the encoder's variable rate modes.
const VARIABLE_RATE_MODE_5: u32 = 5;
const AUDIO_MUX_VERSION_2: u32 = 2;
const SIG_EXPLICIT_BW_COMPATIBLE: u32 = 1;
/// The stream's configuration is sent with every frame.
const HEADER_EVERY_FRAME: u32 = 1;
const AFTERBURNER_ON: u32 = 1;
// PipeWire 1.6.9 spa/plugins/bluez5/a2dp-codec-aac.c, in the order of its calls: the object type every sink must take (336-337), rate and channels (357-361), variable rate mode 5 by default for a sink that allows one (288-289, 365-367), LATM (372-376, 407-411), the two rates (382-394) and the afterburner (415).
const AAC: [Setting; 11] = [
    Setting(AACENC_AOT, AOT_MP2_AAC_LC),
    Setting(AACENC_SAMPLERATE, RATE_HZ),
    Setting(AACENC_CHANNELMODE, MODE_2),
    Setting(AACENC_BITRATEMODE, VARIABLE_RATE_MODE_5),
    Setting(AACENC_AUDIOMUXVER, AUDIO_MUX_VERSION_2),
    Setting(AACENC_SIGNALING_MODE, SIG_EXPLICIT_BW_COMPATIBLE),
    Setting(AACENC_BITRATE, AAC_BIT_RATE),
    Setting(AACENC_PEAK_BITRATE, AAC_PEAK_BIT_RATE),
    Setting(AACENC_TRANSMUX, TT_MP4_LATM_MCP1),
    Setting(AACENC_HEADER_PERIOD, HEADER_EVERY_FRAME),
    Setting(AACENC_AFTERBURNER, AFTERBURNER_ON),
];

/// One codec of the bench: the library that encodes and decodes, and what `PipeWire` sets on it.
#[derive(Debug, Clone, Copy)]
struct Codec {
    key: &'static str,
    /// The step of the tool that drives the library.
    verb: &'static str,
    library: Library,
    settings: &'static [Setting],
    /// The file of `PipeWire` the settings are read from.
    source: &'static str,
    /// The sink those settings are for, which is assumed: what the pods offer is not read.
    sink: fn() -> String,
}

fn sbc_xq_sink() -> String {
    format!(
        "a sink that offers {RATE_HZ} Hz, dual channel and a bitpool of {SBC_XQ_BITPOOL} or more"
    )
}

fn aac_sink() -> String {
    format!(
        "a sink that offers {RATE_HZ} Hz, allows a variable rate, states {AAC_BIT_RATE} bit/s or no rate and takes packets of {MTU_BYTES} bytes"
    )
}

const CODECS: [Codec; 2] = [
    Codec {
        key: "sbc_xq",
        verb: "sbc",
        library: LIBSBC,
        settings: &SBC_XQ,
        source: "a2dp-codec-sbc.c",
        sink: sbc_xq_sink,
    },
    Codec {
        key: "aac",
        verb: "aac",
        library: LIBFDK_AAC,
        settings: &AAC,
        source: "a2dp-codec-aac.c",
        sink: aac_sink,
    },
];

impl Codec {
    fn settings(&self) -> Vec<String> {
        let pair = |Setting(name, value): &Setting| format!("{name}={value}");
        self.settings.iter().map(pair).collect()
    }
}

/// What `probe bench codec` takes.
#[derive(Debug, Args)]
pub(crate) struct CodecArgs {
    /// The cache `probe bench setup` made.
    #[arg(long)]
    cache: PathBuf,
    /// The table to write, a file that does not exist yet.
    #[arg(long)]
    out: PathBuf,
    /// How many times each excerpt goes through each codec and the scorer; a row holds the least and the greatest score.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(2..=9))]
    runs: u8,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CodecError {
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("scoring {}: {source}", .degraded.display())]
    Scorer {
        degraded: PathBuf,
        source: ScorerError,
    },
    #[error("{} exists, and a table is never written over", .0.display())]
    OutExists(PathBuf),
    #[error("could not write {}: {source}", .path.display())]
    Out { path: PathBuf, source: io::Error },
    #[error("could not make a directory for the decodes: {0}")]
    Work(io::Error),
    #[error(
        "the interpreter of the cache did not list its packages as a name and a version a line; it printed: {0}"
    )]
    Versions(String),
}

/// One step of the tool, run by the interpreter of the cache in its isolated mode, which reads no `PYTHON` variable and no user site directory.
fn tool(cache: &Cache, verb: &str) -> Command {
    let mut command = Command::new(cache.python());
    command.args(["-I", "-c", TOOL, verb]);
    command
}

fn is_word(text: &str) -> bool {
    let plain = |character: char| character.is_ascii_alphanumeric() || "-_.+!".contains(character);
    !text.is_empty() && text.chars().all(plain)
}

// Sample input: `python\t3.13.5` and then `visqol-python\t3.8.0`, each a line.
fn tool_rows(said: &str) -> Result<Vec<Row>, CodecError> {
    let row = |line: &str| match line.split_once('\t') {
        Some((name, version)) if is_word(name) && is_word(version) => Ok(Row::new(
            &format!("tool_{name}"),
            &version,
            "version",
            "in the cache",
        )),
        Some(_) | None => {
            let shown = line.chars().filter(|character| !character.is_control());
            Err(CodecError::Versions(shown.take(LINE_CHARS).collect()))
        }
    };
    let rows: Vec<Row> = said.lines().map(row).collect::<Result<_, _>>()?;
    if rows.is_empty() {
        return Err(CodecError::Versions(String::new()));
    }
    Ok(rows)
}

fn pin_row(prefix: &str, pin: &Pin, note: &str) -> Row {
    let key = format!("{prefix}_{}", pin.name);
    let note = format!("sha256 {}{note}", pin.sha256);
    Row::new(&key, &pin.file, "file", &note)
}

/// The least and the greatest of `figures`, at the six decimals the scorer prints.
fn range(figures: impl Iterator<Item = f64>) -> String {
    let ends = (f64::INFINITY, f64::NEG_INFINITY);
    let (least, most) = figures.fold(ends, |(least, most), figure| {
        (least.min(figure), most.max(figure))
    });
    format!("{least:.6} to {most:.6}")
}

fn score_rows(codec: &str, track: &str, scores: &[Score]) -> [Row; 2] {
    let runs = format!("{} runs", scores.len());
    let row = |figure: &str, unit, of: fn(&Score) -> f64| {
        let key = format!("visqol_{figure}_{codec}_{track}");
        Row::new(&key, &range(scores.iter().map(of)), unit, &runs)
    };
    [
        row("mos", "MOS-LQO", |score| score.mos_lqo),
        row("vnsim", "NSIM", |score| score.vnsim),
    ]
}

/// The rows of one track: its file, then the scores of its excerpt against itself and through each codec.
fn track_rows(cache: &Cache, track: &Track, work: &Path, runs: u8) -> Result<Vec<Row>, CodecError> {
    let name = track.pin.name;
    let python = cache.python();
    let reference = work.join(format!("{name}.wav"));
    let mut excerpt = tool(cache, "excerpt");
    excerpt.arg(cache.download(&track.pin));
    excerpt.args([track.start_s, EXCERPT_S, RATE_HZ].map(|number| number.to_string()));
    output_of(excerpt.arg(&reference))?;
    let cut = format!("; {EXCERPT_S} s from {} s at {RATE_HZ} Hz", track.start_s);
    let mut rows = vec![pin_row("track", &track.pin, &cut)];
    let scored = |degraded: &Path, pass: &dyn Fn() -> Result<(), ToolError>| {
        let once = |_| -> Result<Score, CodecError> {
            pass()?;
            visqol::audio(&python, &reference, degraded).map_err(|source| CodecError::Scorer {
                degraded: degraded.to_owned(),
                source,
            })
        };
        (0..runs).map(once).collect::<Result<Vec<Score>, _>>()
    };
    let against_itself = scored(&reference, &|| Ok(()))?;
    rows.extend(score_rows(REFERENCE, name, &against_itself));
    for codec in &CODECS {
        let degraded = work.join(format!("{name}.{}.wav", codec.key));
        let pass = || {
            let mut through = tool(cache, codec.verb);
            through.arg(cache.library(&codec.library));
            through.arg(&reference).arg(&degraded);
            output_of(through.args(codec.settings())).map(drop)
        };
        rows.extend(score_rows(codec.key, name, &scored(&degraded, &pass)?));
    }
    Ok(rows)
}

/// Scores every track and writes the table, which is also what the command prints.
pub(crate) fn run(args: &CodecArgs) -> Result<String, CodecError> {
    let cache = Cache::open(&args.cache)?;
    let out = |source| CodecError::Out {
        path: args.out.clone(),
        source,
    };
    if args.out.try_exists().map_err(out)? {
        return Err(CodecError::OutExists(args.out.clone()));
    }
    let work = tempfile::tempdir().map_err(CodecError::Work)?;
    let listed = output_of(&mut tool(&cache, "versions"))?;
    let mut rows = tool_rows(&String::from_utf8_lossy(&listed))?;
    rows.extend(
        CODECS
            .iter()
            .map(|codec| pin_row("pin", &codec.library.pin, "")),
    );
    rows.extend(CODECS.iter().map(|codec| {
        let key = format!("settings_{}", codec.key);
        let note = format!(
            "as PipeWire 1.6.9 sets them in {} for {}; assumed, not read from the pods",
            codec.source,
            (codec.sink)()
        );
        Row::new(&key, &codec.settings().join(" "), "settings", &note)
    }));
    for track in &TRACKS {
        rows.extend(track_rows(&cache, track, work.path(), args.runs)?);
    }
    let table = table(&rows);
    write_new(&args.out, &table).map_err(out)?;
    Ok(table)
}

/// Support for the test of the built command: the rows and the settings alone, on fixed figures.
#[cfg(test)]
mod tests {
    use super::super::super::analyse::report::table;
    use super::{AAC_PEAK_BIT_RATE, CODECS, CodecError, Score, score_rows, tool_rows};

    #[test]
    fn a_row_holds_the_least_and_the_greatest_of_its_runs() {
        let scores = [(4.7237, 0.9951), (4.72, 0.9962), (4.731_46, 0.99)]
            .map(|(mos_lqo, vnsim)| Score { mos_lqo, vnsim });
        assert_eq!(
            table(&score_rows("aac", "funk", &scores)),
            "key\tvalue\tunit\tnote\n\
             visqol_mos_aac_funk\t4.720000 to 4.731460\tMOS-LQO\t3 runs\n\
             visqol_vnsim_aac_funk\t0.990000 to 0.996200\tNSIM\t3 runs\n"
        );
    }

    #[test]
    fn the_peak_rate_is_what_the_packet_assumed_holds() {
        assert_eq!(AAC_PEAK_BIT_RATE, 376_875);
        let aac = CODECS[1].settings().join(" ");
        assert!(
            aac.contains("0x0102=5 ") && aac.contains("0x0207=376875 "),
            "{aac}"
        );
    }

    #[test]
    fn a_tool_is_a_name_and_a_version_and_nothing_else_is_a_row() {
        let rows = tool_rows("python\t3.13.5\nvisqol-python\t3.8.0\n").unwrap();
        assert_eq!(
            table(&rows),
            "key\tvalue\tunit\tnote\n\
             tool_python\t3.13.5\tversion\tin the cache\n\
             tool_visqol-python\t3.8.0\tversion\tin the cache\n"
        );
        for not_a_list in ["", "python 3.13.5\n", "numpy\t2.5.5\ta\n", "\t1\n", "a\t\n"] {
            let refused = tool_rows(not_a_list);
            assert!(
                matches!(refused, Err(CodecError::Versions(_))),
                "{not_a_list:?}"
            );
        }
    }
}
