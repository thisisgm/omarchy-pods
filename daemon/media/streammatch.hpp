#pragma once

#include <QString>

// Category of a PulseAudio / PipeWire sink input stream.
enum class StreamPlaybackCategory {
    ActivePlayback,
    CorkedPlayback,
    Ignored
};

// Check if a media.role string represents an event, notification, or alert sound
// that should not trigger media playback handoff.
inline bool isIgnoredMediaRole(const QString &role)
{
    if (role.isEmpty()) {
        return false;
    }
    const QString r = role.trimmed().toLower();
    return (r == QStringLiteral("event") ||
            r == QStringLiteral("notification") ||
            r == QStringLiteral("alert") ||
            r == QStringLiteral("test") ||
            r == QStringLiteral("announcement"));
}

// Classify a sink-input stream given its corked status, mute status, and media role.
inline StreamPlaybackCategory classifyStream(bool corked, bool muted, const QString &mediaRole)
{
    if (isIgnoredMediaRole(mediaRole)) {
        return StreamPlaybackCategory::Ignored;
    }
    if (muted) {
        return StreamPlaybackCategory::Ignored;
    }
    if (corked) {
        return StreamPlaybackCategory::CorkedPlayback;
    }
    return StreamPlaybackCategory::ActivePlayback;
}

struct StreamCounts {
    int active = 0;
    int corked = 0;
    int total = 0;
};

// Evaluate combined media state from MPRIS state (0=Playing, 1=Paused, 2=Stopped)
// and PipeWire stream playback flags.
enum class CombinedMediaState {
    Playing = 0,
    Paused = 1,
    Stopped = 2
};

inline CombinedMediaState computeCombinedMediaState(CombinedMediaState mprisState, bool streamPlaying, bool streamExplicitlyPaused)
{
    if (mprisState == CombinedMediaState::Playing || streamPlaying) {
        return CombinedMediaState::Playing;
    }
    if (mprisState == CombinedMediaState::Paused || streamExplicitlyPaused) {
        return CombinedMediaState::Paused;
    }
    return CombinedMediaState::Stopped;
}
