#pragma once

#include <QString>
#include <QtGlobal>

// When the hi-res uplink runs, ported from the transition handling in librepods PR 888.
// Free of Bluetooth and PulseAudio types so every decision is testable on its own.
namespace OpenPods::HiRes
{
    inline constexpr int kPollMs = 400;
    // No audio for this long while worn means the stream stalled: STOP, then START.
    inline constexpr int kStallMs = 2000;
    inline constexpr int kMaxBackoffMs = 30000;
    // After this many restarts without audio the headset profile takes over the same source.
    inline constexpr int kMaxFailedRestarts = 2;
    // The headset profile costs playback quality, so it is left soon after the recorder.
    inline constexpr int kFallbackIdleMs = 3000;
    // Apps close and reopen the mic often, and every STOP costs an audible A2DP reset.
    inline constexpr int kRecorderGoneGraceMs = 60000;
    // Adjusting the buds reports both out for a moment, which must not stop or move the mic.
    inline constexpr int kNotWornGraceMs = 4000;
    // A START sent before the ear report, which ends the handshake, is ignored by the AirPods.
    inline constexpr int kEarReportWaitMs = 3000;
    inline constexpr int kA2dpResetDelayMs = 800;
    // A short drop that reconnects within this changes nothing at all.
    inline constexpr int kDisconnectParkDelayMs = 15000;
    // Level meters record at a few hundred Hz and are not recorders.
    inline constexpr quint32 kMinRecorderRate = 1000;

    enum class CaptureAction { Start, Keep, Restart, Stop, Idle };

    inline CaptureAction captureAction(bool enabled, bool recording, bool worn, bool capturing, bool stalled)
    {
        const bool want = enabled && recording && worn;
        if (want && !capturing) return CaptureAction::Start;
        if (want && stalled) return CaptureAction::Restart;
        if (want) return CaptureAction::Keep;
        return capturing ? CaptureAction::Stop : CaptureAction::Idle;
    }

    inline int nextBackoffMs(int currentMs)
    {
        return qMin(currentMs * 2, kMaxBackoffMs);
    }

    // The parked state is one line: the mic made default while the AirPods could not deliver.
    // It outlives a reboot because the session manager remembers that default across one.
    inline QString serializeParked(const QString &fallback)
    {
        return fallback.isEmpty() ? QString() : QStringLiteral("default\t%1\n").arg(fallback);
    }

    inline QString parseParked(const QString &text)
    {
        for (const QString &line : text.split('\n')) {
            const QStringList fields = line.split('\t');
            if (fields.size() == 2 && fields[0] == QStringLiteral("default") && !fields[1].isEmpty()) {
                return fields[1];
            }
        }
        return QString();
    }
}
