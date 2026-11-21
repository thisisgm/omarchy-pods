#pragma once

#include <QString>

// What PulseAudio could tell us about a capture on the AirPods.
enum class CaptureState { Idle, Live, Unknown };

// Sample inputs: "bluez_input.AA:BB:CC:DD:EE:FF" (WirePlumber loopback) and
// "bluez_input.AA_BB_CC_DD_EE_FF.0" (direct node), against a "AA_BB_CC_DD_EE_FF" address.
inline bool sourceNamesAddress(const QString &sourceName, const QString &macAddress)
{
    if (sourceName.isEmpty() || macAddress.isEmpty()) {
        return false;
    }
    return QString(sourceName).remove(':').remove('_').contains(QString(macAddress).remove(':').remove('_'));
}

// Counted separately from the activation attempts, because a call that outlasts them must not
// spend that budget, and only Unknown means PulseAudio failed to answer.
inline int unansweredChecksAfter(CaptureState state, int unanswered)
{
    return state == CaptureState::Unknown ? unanswered + 1 : 0;
}

// Releasing the card under a live capture drops the headset profiles the call's mic is on, and
// Unknown is not evidence the mic is free.
inline bool mayReleaseCard(CaptureState state)
{
    return state == CaptureState::Idle;
}
