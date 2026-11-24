//! What the codec bench demands before it does its work, and of its table before it passes.

use std::ffi::OsString;

const REFERENCE_ROWS: &str = "visqol_mos_reference_";
const CODECS: [&str; 2] = ["sbc_xq", "aac"];
/// Under the least score of the first measured run, 4.63, and far over what a decode that is not the music scores.
const LEAST_MOS: f64 = 4.5;

// Sample input: `visqol_mos_aac_funk\t4.723734 to 4.723734\tMOS-LQO\t2 runs`, read for its key as 4.723734 and 4.723734.
fn range(table: &str, key: &str) -> Result<(f64, f64), String> {
    let value = table.lines().find_map(|line| {
        let mut fields = line.split('\t');
        (fields.next() == Some(key))
            .then(|| fields.next())
            .flatten()
    });
    let ends = value.and_then(|value| value.split_once(" to "));
    let numbers = ends.and_then(|(least, most)| Some((least.parse().ok()?, most.parse().ok()?)));
    numbers.ok_or_else(|| format!("the table has no row {key} with a range"))
}

/// Does `work` only when Cargo asked for a bench, and says whether it did: Cargo then gives the program `--bench`, and a test run of every target starts it without.
// Sample input: `codec-0123456789abcdef --bench`, each an argument.
pub(crate) fn when_asked(
    mut args: impl Iterator<Item = OsString>,
    work: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    if !args.any(|arg| arg == "--bench") {
        return Ok(false);
    }
    work().map(|()| true)
}

/// Every codec row must lie between the floor and what its excerpt scores against itself.
// Sample input: `visqol_mos_reference_funk\t4.732101 to 4.732101\tMOS-LQO\t2 runs`, then a row like it of each codec for `funk`, each a line.
pub(crate) fn check(table: &str) -> Result<(), String> {
    let keys = table.lines().filter_map(|line| line.split('\t').next());
    let tracks: Vec<&str> = keys
        .filter_map(|key| key.strip_prefix(REFERENCE_ROWS))
        .collect();
    if tracks.is_empty() {
        return Err("the table has no reference row".to_owned());
    }
    for track in tracks {
        let (ceiling, _) = range(table, &format!("{REFERENCE_ROWS}{track}"))?;
        for codec in CODECS {
            let key = format!("visqol_mos_{codec}_{track}");
            let (least, most) = range(table, &key)?;
            if !(least >= LEAST_MOS && least <= most && most <= ceiling) {
                return Err(format!(
                    "{key} is {least} to {most}, outside {LEAST_MOS} to the reference's {ceiling}"
                ));
            }
        }
    }
    Ok(())
}
