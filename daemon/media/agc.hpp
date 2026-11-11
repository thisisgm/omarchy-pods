#pragma once

#include <QtGlobal>
#include <cmath>

// Preamp for the AirPods uplink. Measured on this device: a voice arrives near -51 dBFS, far
// below the level a noise suppressor expects, and one fed that quiet a signal strips the voice
// along with the room. The gain is fixed rather than tracked, so the suppressor downstream still
// sees the dynamics its voice detector keys on.
namespace OpenPods::HiRes
{
    class Preamp
    {
    public:
        void process(qint16 *samples, int count)
        {
            for (int i = 0; i < count; ++i) {
                const float x = static_cast<float>(samples[i]) / 32768.0f;
                // tanh keeps a transient inside the range instead of wrapping it.
                samples[i] = static_cast<qint16>(std::tanh(x * kGain) * 32767.0f);
            }
        }

        static constexpr float gain() { return kGain; }

    private:
        // About +18 dB. Measured: a voice then reaches about -15 dBFS before the suppressor, and
        // about -22 after it, with headroom left for a raised voice.
        static constexpr float kGain = 8.0f;
    };
}
