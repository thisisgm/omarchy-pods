//! The results of a hardware run: one verdict per check, and each gate as a range with its trial count.

/// The outcome of one check, with the lines it reports under its verdict.
#[derive(Debug)]
pub(crate) struct Check {
    pub(crate) name: &'static str,
    pub(crate) passed: bool,
    pub(crate) lines: Vec<String>,
    pub(crate) gates: Vec<Gate>,
}

impl Check {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            passed: true,
            lines: Vec::new(),
            gates: Vec::new(),
        }
    }

    /// The check stays failed whatever passes after it.
    pub(crate) fn fail(&mut self, reason: impl Into<String>) {
        self.passed = false;
        self.lines.push(reason.into());
    }

    pub(crate) fn note(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
}

/// One measured gate: its samples in milliseconds and the bound they are held to, if it has one yet.
#[derive(Debug)]
pub(crate) struct Gate {
    pub(crate) name: &'static str,
    pub(crate) samples_ms: Vec<u64>,
    pub(crate) bound: Option<Bound>,
}

/// At most `at_most_ms` in at least `needed` of the trials.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bound {
    pub(crate) at_most_ms: u64,
    pub(crate) needed: usize,
    pub(crate) of: usize,
}

impl Gate {
    /// True when the gate has no bound, or enough samples fall inside it.
    pub(crate) fn met(&self) -> bool {
        let Some(bound) = self.bound else {
            return true;
        };
        let inside = self
            .samples_ms
            .iter()
            .filter(|&&sample| sample <= bound.at_most_ms)
            .count();
        inside >= bound.needed
    }

    /// The gate as one line: the range, the trial count, the bound and the verdict.
    pub(crate) fn line(&self) -> String {
        let range = range_text(&self.samples_ms);
        match self.bound {
            None => format!("gate {}: {range}, no bound yet", self.name),
            Some(bound) => format!(
                "gate {}: {range}, bound at most {} ms in {} of {}: {}",
                self.name,
                bound.at_most_ms,
                bound.needed,
                bound.of,
                if self.met() { "met" } else { "MISSED" }
            ),
        }
    }
}

/// `110 to 200 ms over 10`, or `no samples`.
pub(crate) fn range_text(samples_ms: &[u64]) -> String {
    match (samples_ms.iter().min(), samples_ms.iter().max()) {
        (Some(low), Some(high)) => format!("{low} to {high} ms over {}", samples_ms.len()),
        _ => "no samples".to_owned(),
    }
}

/// True when every check passed and every bounded gate was met.
pub(crate) fn all_passed(checks: &[Check]) -> bool {
    checks
        .iter()
        .all(|check| check.passed && check.gates.iter().all(Gate::met))
}

/// The report lines for every check, in the order the checks ran.
pub(crate) fn lines(checks: &[Check]) -> Vec<String> {
    let mut out = Vec::new();
    for check in checks {
        let verdict = if check.passed { "passed" } else { "FAILED" };
        out.push(format!("{}: {verdict}", check.name));
        out.extend(check.lines.iter().map(|line| format!("  {line}")));
        out.extend(check.gates.iter().map(|gate| format!("  {}", gate.line())));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Bound, Check, Gate, all_passed, range_text};

    fn gate(samples_ms: Vec<u64>, at_most_ms: u64, needed: usize) -> Gate {
        Gate {
            name: "G1b",
            samples_ms,
            bound: Some(Bound {
                at_most_ms,
                needed,
                of: 5,
            }),
        }
    }

    #[test]
    fn a_range_names_its_ends_and_its_count() {
        assert_eq!(range_text(&[200, 110, 150]), "110 to 200 ms over 3");
        assert_eq!(range_text(&[]), "no samples");
    }

    #[test]
    fn a_gate_is_met_when_enough_samples_fall_inside() {
        assert!(gate(vec![100, 200, 300, 400, 600], 500, 4).met());
        assert!(gate(vec![500, 500, 500, 500, 900], 500, 4).met());
        assert!(!gate(vec![100, 200, 300, 600, 600], 500, 4).met());
        assert!(gate(vec![1, 2], 500, 4).line().ends_with("MISSED"));
    }

    #[test]
    fn a_missed_gate_fails_the_run_even_when_its_check_passed() {
        let mut check = Check::new("C4 reconnect");
        check.gates.push(gate(vec![900; 5], 500, 4));
        assert!(!all_passed(&[check]));
    }
}
