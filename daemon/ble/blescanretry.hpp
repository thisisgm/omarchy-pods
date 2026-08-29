#pragma once

#include <algorithm>

namespace BleScanRetry
{
// The delay doubles from one second, with a limit of 32 seconds.
inline constexpr int baseDelayMs = 1000;
inline constexpr int maxDoublings = 6;

inline int delayMs(int attempt)
{
    const int boundedAttempt = std::clamp(attempt, 1, maxDoublings);
    return baseDelayMs * (1 << (boundedAttempt - 1));
}

// One counter for the process. A reset gives the next failure the short delay again.
class Ladder
{
public:
    int nextDelayMs() { return delayMs(++m_attempts); }
    void reset() { m_attempts = 0; }
    int attempts() const { return m_attempts; }

private:
    int m_attempts = 0;
};
}
