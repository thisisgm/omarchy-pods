//! `ViSQOL` as the scorer: one run of the pinned package over a reference and a degraded file, read from what it prints.

use std::path::Path;
use std::process::Command;

use super::{ToolError, output_of};

/// The module run with `-m`; with no mode named it scores in audio mode, at 48 kHz.
const MODULE: &str = "visqol";
const MOS_LABEL: &str = "MOS-LQO:";
const VNSIM_LABEL: &str = "VNSIM:";
/// The scale a mean opinion score lies on.
const MOS_RANGE: (f64, f64) = (1.0, 5.0);
/// The scale a similarity lies on.
const VNSIM_RANGE: (f64, f64) = (-1.0, 1.0);
/// The most of the scorer's first line an error carries.
const FIRST_LINE_CHARS: usize = 200;

/// What `ViSQOL` made of one pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Score {
    /// The mapped listening quality, which saturates near 4.73 in audio mode.
    pub(crate) mos_lqo: f64,
    /// The mean similarity of the two spectrograms the score is mapped from.
    pub(crate) vnsim: f64,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ScorerError {
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("the scorer printed no {label} line with a number from {} to {}; it began: {first_line}", .range.0, .range.1)]
    NoFigure {
        label: &'static str,
        range: (f64, f64),
        first_line: String,
    },
}

/// The number after `label`, when it is one and lies on the scale.
fn figure(said: &str, label: &'static str, range: (f64, f64)) -> Result<f64, ScorerError> {
    let (least, most) = range;
    let labelled = said.lines().find_map(|line| line.strip_prefix(label));
    let number = labelled.and_then(|rest| rest.trim().parse::<f64>().ok());
    // NaN fails both comparisons and an infinity fails one, so a number that is not finite is on no scale.
    number
        .filter(|number| *number >= least && *number <= most)
        .ok_or_else(|| ScorerError::NoFigure {
            label,
            range,
            first_line: said
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|character| !character.is_control())
                .take(FIRST_LINE_CHARS)
                .collect(),
        })
}

// Sample input: `MOS-LQO:      4.349722` and then `VNSIM:        0.962770`, each a line.
fn parse(said: &str) -> Result<Score, ScorerError> {
    Ok(Score {
        mos_lqo: figure(said, MOS_LABEL, MOS_RANGE)?,
        vnsim: figure(said, VNSIM_LABEL, VNSIM_RANGE)?,
    })
}

/// Scores `degraded` against `reference` in audio mode with the interpreter of the cache.
pub(crate) fn audio(
    python: &Path,
    reference: &Path,
    degraded: &Path,
) -> Result<Score, ScorerError> {
    let mut scorer = Command::new(python);
    scorer.args(["-I", "-m", MODULE, "-r"]).arg(reference);
    let said = output_of(scorer.arg("-d").arg(degraded))?;
    parse(&String::from_utf8_lossy(&said))
}

/// Support for the test of the built command: the reader alone, on what the pinned scorer printed and on what it must not take.
#[cfg(test)]
mod tests {
    use super::{Score, ScorerError, parse};

    /// What the pinned scorer printed for a pair of the conformance set of the reference implementation.
    const PRINTED: &str = "MOS-LQO:      4.349722\nVNSIM:        0.962770\n";

    #[test]
    fn the_two_figures_are_read_from_what_the_scorer_prints() {
        let score = parse(PRINTED).unwrap();
        assert_eq!(
            score,
            Score {
                mos_lqo: 4.349_722,
                vnsim: 0.962_770
            }
        );
        let verbose = format!("Reference: a.wav\n{PRINTED}Patches: 12\n");
        assert_eq!(parse(&verbose).unwrap(), score);
    }

    #[test]
    fn a_figure_that_is_missing_or_off_its_scale_is_refused_by_name() {
        let cases = [
            ("", "MOS-LQO:"),
            ("Traceback (most recent call last):\n", "MOS-LQO:"),
            ("MOS-LQO: high\nVNSIM: 0.9\n", "MOS-LQO:"),
            ("MOS-LQO: 0.99\nVNSIM: 0.9\n", "MOS-LQO:"),
            ("MOS-LQO: 5.01\nVNSIM: 0.9\n", "MOS-LQO:"),
            ("MOS-LQO: NaN\nVNSIM: 0.9\n", "MOS-LQO:"),
            ("MOS-LQO: inf\nVNSIM: 0.9\n", "MOS-LQO:"),
            ("MOS-LQO: 4.2\n", "VNSIM:"),
            ("MOS-LQO: 4.2\nVNSIM: 1.01\n", "VNSIM:"),
            ("mos-lqo: 4.2\nVNSIM: 0.9\n", "MOS-LQO:"),
        ];
        for (said, missing) in cases {
            let Err(ScorerError::NoFigure {
                label, first_line, ..
            }) = parse(said)
            else {
                panic!("{said:?} was read as a score");
            };
            assert_eq!(label, missing, "{said:?}");
            assert_eq!(first_line, said.lines().next().unwrap_or_default());
        }
    }

    #[test]
    fn the_first_line_of_an_error_is_one_cell_and_bounded() {
        let said = format!("a\tb{}\nMOS-LQO: 4.2\n", "x".repeat(500));
        let Err(error) = parse(&said) else {
            panic!("read as a score");
        };
        let text = error.to_string();
        assert!(text.contains("it began: ab"), "{text}");
        assert!(!text.contains('\t') && text.len() < 400, "{text}");
    }
}
