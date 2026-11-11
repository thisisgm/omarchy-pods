#pragma once

#include <QtGlobal>
#include <cmath>

// Sample conversion for the AAC-ELD uplink, kept out of the decoder so it is testable where
// tests link Qt6::Core alone.
namespace OpenPods::HiRes
{
    // The AirPods uplink: AOT 39, mono, 480-sample frames of 7.5 ms, decoded at 64 kHz.
    inline constexpr int kEldSampleRate = 64000;
    inline constexpr int kEldFrameSamples = 480;
    inline constexpr int kEldChannels = 1;

    // libavcodec hands back planar floats; module-pipe-source is fed s16, so each sample is
    // clamped before scaling. Without the clamp an overdriven frame wraps to the opposite sign.
    inline qint16 floatToS16(float sample)
    {
        const float clamped = sample < -1.0f ? -1.0f : (sample > 1.0f ? 1.0f : sample);
        return static_cast<qint16>(std::lround(clamped * 32767.0f));
    }
}
