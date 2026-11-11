#pragma once

namespace ScanDuty
{
// A continuous LE scan holds the radio, and BlueZ answers a connect request on the same
// controller, so a BLE HID device reconnecting after resume can be starved indefinitely.
// Scanning half the time leaves gaps wide enough for that reconnect to complete.
inline constexpr int windowMs = 6000;
inline constexpr int gapMs = 6000;

// Resume is the worst moment to take the radio back: the controller has just reset and a
// bonded HID device re-associates in the first seconds after it does.
inline constexpr int resumeSettleMs = 10000;

enum class Phase
{
    Idle,
    Window,
    Gap
};

class Cycle
{
public:
    void start() { m_phase = Phase::Window; }
    void stop() { m_phase = Phase::Idle; }

    bool active() const { return m_phase != Phase::Idle; }
    Phase phase() const { return m_phase; }

    // True when the caller must time a gap. A stop that raced the agent's finished signal
    // leaves the cycle idle, and restarting it there would resume a scan nobody asked for.
    bool windowFinished()
    {
        if (m_phase != Phase::Window) {
            return false;
        }
        m_phase = Phase::Gap;
        return true;
    }

    // True when the caller must scan again.
    bool gapFinished()
    {
        if (m_phase != Phase::Gap) {
            return false;
        }
        m_phase = Phase::Window;
        return true;
    }

private:
    Phase m_phase = Phase::Idle;
};
}
