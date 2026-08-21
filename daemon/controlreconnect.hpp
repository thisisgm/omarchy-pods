#pragma once

#include <algorithm>
#include <cstdint>

namespace ControlReconnect
{
enum class State
{
    Idle,
    Waiting,
    ProbingBlueZ,
    ConnectingSocket
};

// Doubling from one second, capped at 16s so a long recovery polls rather than sleeps through the reconnect.
inline constexpr int baseDelayMs = 1000;
inline constexpr int maxDoublings = 5;
inline constexpr int maxJitterMs = 499;
inline constexpr int jitterRangeMs = maxJitterMs + 1;

// Short enough that a real profile rebuild is still in progress when the first probe runs.
inline constexpr int firstDelayMs = 750;

// Ten retries on the ladder above is roughly two minutes, longer than any profile rebuild measured here.
inline constexpr int connectedAttemptLimit = 10;

// Fallback poll once even the connected-attempt ladder above is exhausted and nothing else
// is left to notify us: BlueZ can settle into Device1::Connected without ever emitting
// another PropertiesChanged transition, so a session that gave up would otherwise never be
// retried. Slow enough to stay a safety net rather than a second retry loop.
inline constexpr int watchdogIntervalMs = 30000;

inline int delayMs(int attempt, int jitterMs)
{
    const int boundedAttempt = std::clamp(attempt, 1, maxDoublings);
    const int boundedJitter = std::clamp(jitterMs, 0, maxJitterMs);
    return baseDelayMs * (1 << (boundedAttempt - 1)) + boundedJitter;
}

inline bool hasAttemptRemaining(int completedAttempts, int maximumAttempts)
{
    return completedAttempts < std::max(0, maximumAttempts);
}

class Session
{
public:
    void begin(bool bleScanWasActive)
    {
        reset();
        m_state = State::Waiting;
        m_restoreBleScan = bleScanWasActive;
    }

    bool isActive() const { return m_state != State::Idle; }
    State state() const { return m_state; }
    int absentProbes() const { return m_absentProbes; }
    int connectedAttempts() const { return m_connectedAttempts; }

    std::uint64_t beginProbe()
    {
        m_state = State::ProbingBlueZ;
        return ++m_generation;
    }

    bool acceptsProbe(std::uint64_t generation) const
    {
        return m_state == State::ProbingBlueZ && generation == m_generation;
    }

    void beginConnection()
    {
        m_state = State::ConnectingSocket;
        ++m_generation;
    }

    bool prepareRetry(int maximumAttempts, bool deviceStillConnected)
    {
        if (!isActive()) {
            return false;
        }
        // A device BlueZ still reports connected is worth a longer ladder than one it says has gone.
        if (deviceStillConnected) {
            if (!hasAttemptRemaining(m_connectedAttempts, connectedAttemptLimit)) {
                return false;
            }
            ++m_connectedAttempts;
        } else {
            if (!hasAttemptRemaining(m_absentProbes, maximumAttempts)) {
                return false;
            }
            ++m_absentProbes;
        }
        m_state = State::Waiting;
        ++m_generation;
        return true;
    }

    bool complete()
    {
        const bool restoreBleScan = m_restoreBleScan;
        reset();
        return restoreBleScan;
    }

    void cancel() { reset(); }

private:
    void reset()
    {
        m_state = State::Idle;
        m_absentProbes = 0;
        m_connectedAttempts = 0;
        m_restoreBleScan = false;
        ++m_generation;
    }

    State m_state = State::Idle;
    int m_absentProbes = 0;
    int m_connectedAttempts = 0;
    bool m_restoreBleScan = false;
    std::uint64_t m_generation = 0;
};
}
