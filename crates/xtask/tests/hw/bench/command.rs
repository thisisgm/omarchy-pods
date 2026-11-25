//! Command setup and timing overrides for the hardware CLI fixture.

use std::process::{Command, Output, Stdio};

use super::{Bench, RUN_DEADLINE, finish};

const QUICK_IDLE_MS: &str = "250";
pub(crate) const QUICK_BURST_MS: &str = "400";
pub(crate) const QUICK_CONNECT_MS: &str = "800";

impl Bench {
    pub(crate) fn command(&self, idle_window: Option<[&str; 2]>, extra: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .args(["hw", "link", "--binary"])
            .arg(self.path("bin/librepods"))
            .args(idle_window.unwrap_or(["--idle-window-ms", QUICK_IDLE_MS]))
            .args(extra)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.path("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("HOME", self.path("home"))
            .env("TMPDIR", self.path("tmp"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .env("STUB", self.root.path())
            .current_dir(self.root.path())
            .stdin(Stdio::null());
        command
    }

    pub(crate) fn run(&self, extra: &[&str]) -> Output {
        self.run_with_windows(None, Some(QUICK_BURST_MS), extra)
    }

    pub(crate) fn run_with_windows(
        &self,
        idle_window: Option<[&str; 2]>,
        burst_ms: Option<&str>,
        extra: &[&str],
    ) -> Output {
        let mut command = self.command(idle_window, extra);
        if let Some(milliseconds) = burst_ms {
            command.args(["--burst-window-ms", milliseconds]);
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        finish(self.spawn(command), RUN_DEADLINE)
    }
}
