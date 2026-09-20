#pragma once

#include <QtGlobal>
#include <optional>

class MediaConnectPolicy
{
public:
    static constexpr qint64 pauseGraceMs = 10000;
    static constexpr qint64 retryMs = 15000;
    static constexpr qint64 cooldownMs = 60000;
    static constexpr int maxAttempts = 2;

    bool observe(qint64 now, std::optional<bool> playing, bool connected, bool busy = false)
    {
        // An unreadable player is not evidence that playback stopped.
        if (!playing.has_value()) {
            m_playing = false;
            m_pausedSince = -1;
            return false;
        }
        if (m_pausedSince >= 0 && now - m_pausedSince >= pauseGraceMs) {
            m_attempts = 0;
            m_satisfied = false;
            m_episodeActive = false;
        }
        if (!*playing) {
            m_playing = false;
            if (m_pausedSince < 0) m_pausedSince = now;
            return false;
        }
        m_pausedSince = -1;
        m_episodeActive = true;
        m_playing = true;
        if (connected) m_satisfied = true;
        if (busy || m_satisfied || m_attempts >= maxAttempts || now < m_retryAfter) return false;
        const qint64 delay = m_attempts == 0 ? cooldownMs : retryMs;
        if (m_lastAttempt >= 0 && now - m_lastAttempt < delay) return false;
        m_lastAttempt = now;
        ++m_attempts;
        return true;
    }

    bool connectionStillWanted() const { return m_playing && !m_satisfied; }
    void satisfy() { if (m_episodeActive) m_satisfied = true; }
    void connectionFinished(qint64 now) { m_retryAfter = now + retryMs; }

private:
    bool m_playing = false;
    qint64 m_pausedSince = -1;
    qint64 m_lastAttempt = -1;
    qint64 m_retryAfter = 0;
    int m_attempts = 0;
    bool m_satisfied = false;
    bool m_episodeActive = false;
};
