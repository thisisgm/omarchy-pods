#pragma once

namespace ScanDuty
{
// An active discovery holds the controller, and the kernel's accept-list scan is
// what catches a bonded BLE HID device when it wakes, so discovery has to leave gaps.
inline constexpr int scanWindowMs = 2000;

// Wide enough for the accept-list scan to catch a waking mouse, short enough that
// pods in the case still report battery on the timescale a bar widget is read at.
inline constexpr int idleWindowMs = 8000;

// Resume used to restart discovery after 2s, which is the window bonded HID uses
// to re-associate. Hold the radio idle until that attempt has had a chance.
inline constexpr int resumeHoldOffMs = 10000;

enum class Phase
{
    Stopped,
    Scanning,
    Idle
};

// Callers save and restore "is the scan on" across control-link recovery, so that
// answer has to survive a window boundary instead of flapping with the agent.
class Cycle
{
public:
    void request() { m_phase = Phase::Scanning; }
    void cancel() { m_phase = Phase::Stopped; }

    Phase phase() const { return m_phase; }
    bool isRequested() const { return m_phase != Phase::Stopped; }

    bool windowFinished()
    {
        if (m_phase != Phase::Scanning) {
            return false;
        }
        m_phase = Phase::Idle;
        return true;
    }

    bool idleFinished()
    {
        if (m_phase != Phase::Idle) {
            return false;
        }
        m_phase = Phase::Scanning;
        return true;
    }

private:
    Phase m_phase = Phase::Stopped;
};
}
