#pragma once

#include <QString>
#include <optional>

// MPRIS PlaybackStatus: "Playing", "Paused", or "Stopped".
inline std::optional<bool> playerIsPlaying(const QString &status)
{
    if (status == "Playing") return true;
    if (status == "Paused" || status == "Stopped") return false;
    return std::nullopt;
}

inline bool isAutoConnectPlayer(const QString &service)
{
    for (const auto *player : {"chromium", "google-chrome", "spotify"}) {
        const QString name = QStringLiteral("org.mpris.MediaPlayer2.") + QLatin1String(player);
        if (service == name || service.startsWith(name + '.')) return true;
    }
    return false;
}
