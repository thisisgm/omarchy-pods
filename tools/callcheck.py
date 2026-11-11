#!/usr/bin/env python3
"""callcheck: is this machine ready for a call on the AirPods?

Shows the input and output a call would get, a live level meter for the
microphone, and a record-then-play check so you hear yourself as the far side
would. Reads PipeWire through pactl and the AirPods daemon through
librepods-ctl; it changes nothing unless you press a button.
"""
from __future__ import annotations

import json
import math
import shutil
import subprocess
import sys
import wave
from dataclasses import dataclass
from pathlib import Path
from tempfile import NamedTemporaryFile

from PySide6.QtCore import QProcess, Qt, QTimer, Signal
from PySide6.QtGui import QFont, QPalette
from PySide6.QtWidgets import (
    QApplication, QHBoxLayout, QLabel, QProgressBar, QPushButton, QVBoxLayout, QWidget,
)

HIRES_SOURCE = "AirPodsHiRes"
# The same uplink with RNNoise over it, which is the one to use on a call.
CLEAN_SOURCE = "AirPodsClean"
RECORD_SECONDS = 4


def run(args: list[str], timeout: float = 5.0) -> str:
    try:
        done = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.SubprocessError):
        return ""
    return done.stdout if done.returncode == 0 else ""


def write_piano_loop(path: Path) -> None:
    """A four-note arpeggio, each note alternating left and right.

    Synthesised rather than shipped so there is no audio file in the repo, and
    the alternation is what makes a dead channel obvious.
    """
    rate = 48000
    notes = [261.63, 329.63, 392.00, 523.25]  # C4 E4 G4 C5
    seconds = 0.6
    frames = bytearray()
    for index, freq in enumerate(notes):
        for n in range(int(rate * seconds)):
            t = n / rate
            # A struck string: a few harmonics that decay, the higher ones faster.
            value = sum(
                math.sin(2 * math.pi * freq * h * t) * (0.6 ** (h - 1)) * math.exp(-t * (2.5 + h))
                for h in (1, 2, 3)
            )
            sample = int(max(-1.0, min(1.0, value * 0.5)) * 32767)
            left = sample if index % 2 == 0 else 0
            right = sample if index % 2 == 1 else 0
            frames += int(left).to_bytes(2, "little", signed=True)
            frames += int(right).to_bytes(2, "little", signed=True)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(bytes(frames))


@dataclass
class Readiness:
    default_source: str
    default_sink: str
    card_profile: str
    hires_published: bool
    clean_published: bool
    mic_enabled: bool | None
    mic_active: bool | None
    model: str

    @property
    def problems(self) -> list[str]:
        out = []
        if self.default_source.startswith("bluez_input."):
            out.append("The AirPods HFP mic is your input: joining a call drops music to 16 kHz mono.")
        elif self.default_source == HIRES_SOURCE and self.clean_published:
            out.append(f"{CLEAN_SOURCE} exists and suppresses room noise; the raw uplink is selected.")
        elif self.default_source not in (HIRES_SOURCE, CLEAN_SOURCE) and self.hires_published:
            out.append("The AirPods microphone is available but something else is your input.")
        if self.mic_enabled is False:
            out.append("The AirPods microphone is switched off in the panel.")
        if self.card_profile.startswith("headset"):
            out.append("The card is on a headset profile, so playback is the narrowband one.")
        if not self.default_sink.startswith("bluez_output."):
            out.append("Output is not the AirPods.")
        return out


def read_state() -> Readiness:
    status: dict = {}
    ctl = shutil.which("librepods-ctl") or str(Path.home() / ".local/bin/librepods-ctl")
    raw = run([ctl, "status"]) if Path(ctl).exists() else ""
    if raw:
        try:
            status = json.loads(raw)
        except json.JSONDecodeError:
            status = {}

    profile = ""
    card = ""
    for line in run(["pactl", "list", "cards"]).splitlines():
        stripped = line.strip()
        if stripped.startswith("Name: bluez_card"):
            card = stripped
        elif card and stripped.startswith("Active Profile:"):
            profile = stripped.split(":", 1)[1].strip()
            card = ""
    sources = run(["pactl", "list", "sources", "short"])
    return Readiness(
        default_source=run(["pactl", "get-default-source"]).strip(),
        default_sink=run(["pactl", "get-default-sink"]).strip(),
        card_profile=profile,
        hires_published=HIRES_SOURCE in sources,
        clean_published=CLEAN_SOURCE in sources,
        mic_enabled=status.get("hires_mic_enabled"),
        mic_active=status.get("hires_mic_active"),
        model=status.get("model_name", ""),
    )


class LevelMeter(QWidget):
    """Peak level of the default input, sampled by a short pw-record each tick."""

    def __init__(self) -> None:
        super().__init__()
        self.bar = QProgressBar()
        self.bar.setRange(0, 100)
        self.bar.setTextVisible(False)
        self.caption = QLabel("idle")
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(self.bar)
        layout.addWidget(self.caption)
        self._timer = QTimer(self)
        self._timer.timeout.connect(self._sample)
        self._peak = 0.0

    def start(self) -> None:
        self._timer.start(600)

    def stop(self) -> None:
        self._timer.stop()
        self.bar.setValue(0)
        self.caption.setText("idle")

    def _sample(self) -> None:
        # Recorded to stdout and stopped with an interrupt: killing pw-record outright leaves
        # the wav header unfinalised, which reads back as silence.
        try:
            done = subprocess.run(["timeout", "-s", "INT", "0.5", "pw-record",
                                   "--rate", "48000", "--channels", "1", "--", "-"],
                                  capture_output=True, timeout=2.0)
        except (OSError, subprocess.SubprocessError):
            self.caption.setText("pw-record is not available")
            return
        level = peak_of_raw(done.stdout)
        if level is None:
            self.caption.setText("no audio from the input")
            return
        self._peak = max(level, self._peak * 0.6)
        self.bar.setValue(int(min(self._peak, 1.0) * 100))
        dbfs = -99.0 if self._peak <= 0 else 20 * math.log10(self._peak)
        self.caption.setText(f"peak {dbfs:.0f} dBFS" + ("  (speak to test)" if dbfs < -45 else ""))


def peak_of_raw(data: bytes, header: int = 44) -> float | None:
    body = data[header:]
    if len(body) < 2:
        return None
    peak = 0
    for i in range(0, len(body) - 1, 2):
        sample = int.from_bytes(body[i:i + 2], "little", signed=True)
        peak = max(peak, abs(sample))
    return peak / 32768.0


def peak_of(path: Path) -> float | None:
    try:
        with wave.open(str(path)) as w:
            frames = w.readframes(w.getnframes())
            width = w.getsampwidth()
    except (OSError, wave.Error):
        return None
    if not frames or width != 2:
        return None
    peak = 0
    for i in range(0, len(frames) - 1, 2):
        sample = int.from_bytes(frames[i:i + 2], "little", signed=True)
        peak = max(peak, abs(sample))
    return peak / 32768.0


class CallCheck(QWidget):
    def __init__(self) -> None:
        super().__init__()
        self.setWindowTitle("Call check")
        self.setMinimumWidth(520)

        self.verdict = QLabel()
        verdict_font = QFont()
        verdict_font.setPointSize(13)
        verdict_font.setBold(True)
        self.verdict.setFont(verdict_font)
        self.detail = QLabel()
        self.detail.setWordWrap(True)
        self.detail.setTextFormat(Qt.PlainText)

        self.meter = LevelMeter()
        self.record_button = QPushButton(f"Record {RECORD_SECONDS}s, play back")
        self.record_button.clicked.connect(self.record_and_play)
        self.fix_button = QPushButton("Use the AirPods mic")
        self.fix_button.setToolTip("Prefers the noise-suppressed source when it exists")
        self.fix_button.clicked.connect(self.use_hires)
        self.play_button = QPushButton("Play piano loop")
        self.play_button.setCheckable(True)
        self.play_button.toggled.connect(self.toggle_piano)
        self._piano = QProcess(self)
        self._piano.finished.connect(self._piano_finished)
        self._piano_file: Path | None = None

        buttons = QHBoxLayout()
        buttons.addWidget(self.record_button)
        buttons.addWidget(self.play_button)
        buttons.addWidget(self.fix_button)

        layout = QVBoxLayout(self)
        layout.addWidget(self.verdict)
        layout.addWidget(self.detail)
        layout.addSpacing(6)
        layout.addWidget(QLabel("Microphone level"))
        layout.addWidget(self.meter)
        layout.addLayout(buttons)
        layout.addStretch(1)

        self._refresh = QTimer(self)
        self._refresh.timeout.connect(self.refresh)
        self._refresh.start(1500)
        self.refresh()
        self.meter.start()

    def refresh(self) -> None:
        state = read_state()
        problems = state.problems
        palette = self.verdict.palette()
        if problems:
            self.verdict.setText("Not ready")
            palette.setColor(QPalette.WindowText, Qt.red)
        else:
            self.verdict.setText("Ready for a call")
            palette.setColor(QPalette.WindowText, Qt.darkGreen)
        self.verdict.setPalette(palette)

        lines = [
            f"device:   {state.model or 'no AirPods reported'}",
            f"input:    {state.default_source or 'unknown'}",
            f"output:   {state.default_sink or 'unknown'}",
            f"profile:  {state.card_profile or 'no AirPods card'}",
            f"mic:      {'in use' if state.mic_active else 'idle'}"
            + ("" if state.mic_enabled is None else f", switch {'on' if state.mic_enabled else 'off'}"),
        ]
        self.detail.setText("\n".join(lines + [""] + [f"- {p}" for p in problems]))
        best = CLEAN_SOURCE if state.clean_published else HIRES_SOURCE
        if not state.hires_published:
            self.fix_button.setText("AirPods mic unavailable")
            self.fix_button.setEnabled(False)
        elif state.default_source == best:
            # Enabled on purpose: a disabled button reads as broken rather than as nothing to do.
            self.fix_button.setText(f"Mic is {best}")
            self.fix_button.setEnabled(True)
        else:
            self.fix_button.setText(f"Switch to {best}")
            self.fix_button.setEnabled(True)

    def toggle_piano(self, playing: bool) -> None:
        if not playing:
            self.play_button.setText("Play piano loop")
            self._piano.kill()
            return
        if self._piano_file is None:
            self._piano_file = Path(NamedTemporaryFile(suffix=".wav", delete=False).name)
            write_piano_loop(self._piano_file)
        self.play_button.setText("Stop (left, right, left, right)")
        self._start_piano()

    def _start_piano(self) -> None:
        if self._piano_file is not None:
            self._piano.start("pw-play", ["--", str(self._piano_file)])

    # Restarting on exit is the loop: pw-play has no repeat of its own.
    def _piano_finished(self) -> None:
        if self.play_button.isChecked():
            self._start_piano()

    def closeEvent(self, event) -> None:
        self._piano.kill()
        if self._piano_file is not None:
            self._piano_file.unlink(missing_ok=True)
        super().closeEvent(event)

    def use_hires(self) -> None:
        state = read_state()
        wanted = CLEAN_SOURCE if state.clean_published else HIRES_SOURCE
        subprocess.run(["pactl", "set-default-source", wanted], capture_output=True)
        self.refresh()

    def record_and_play(self) -> None:
        self.record_button.setEnabled(False)
        self.meter.stop()
        self.record_button.setText("Recording...")
        QApplication.processEvents()
        path = Path(NamedTemporaryFile(suffix=".wav", delete=False).name)
        subprocess.run(["timeout", str(RECORD_SECONDS), "pw-record", "--rate", "48000",
                        "--channels", "1", "--", str(path)], capture_output=True)
        self.record_button.setText("Playing it back...")
        QApplication.processEvents()
        subprocess.run(["pw-play", "--", str(path)], capture_output=True)
        path.unlink(missing_ok=True)
        self.record_button.setText(f"Record {RECORD_SECONDS}s, play back")
        self.record_button.setEnabled(True)
        self.meter.start()


def main() -> int:
    if not shutil.which("pactl"):
        print("pactl is required", file=sys.stderr)
        return 1
    app = QApplication(sys.argv)
    window = CallCheck()
    window.show()
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())
