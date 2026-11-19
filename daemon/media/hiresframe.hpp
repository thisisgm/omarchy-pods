#pragma once

#include <QByteArray>
#include <QByteArrayView>
#include <functional>

// AACP framing for the AirPods hi-res microphone uplink: message type 0x58,
// subtype 0x0001, carried on the same L2CAP PSM 0x1001 socket the daemon owns.
// Free of Bluetooth and PulseAudio types so the framing is testable on its own.
namespace OpenPods::HiRes
{
    // 22-byte header, then N repetitions of [timestamp u32 LE][length u8][AAC-ELD AU].
    inline constexpr qsizetype kSduHeaderLen = 22;
    inline constexpr qsizetype kAuPrefixLen = 5;

    inline quint16 u16le(QByteArrayView b, qsizetype off)
    {
        return static_cast<quint16>(static_cast<quint8>(b[off]))
             | static_cast<quint16>(static_cast<quint8>(b[off + 1])) << 8;
    }

    // Control frames share message type 0x58, so the subtype is what separates audio from them.
    inline bool isAudioSdu(QByteArrayView sdu)
    {
        return sdu.size() >= 8
            && static_cast<quint8>(sdu[0]) == 0x04
            && static_cast<quint8>(sdu[2]) == 0x04
            && u16le(sdu, 4) == 0x58
            && u16le(sdu, 6) == 0x0001;
    }

    // Walks the access units of one audio SDU and returns how many were passed on. A truncated
    // tail ends the walk: an SDU read into too small a buffer must not yield a partial unit.
    // The callback is not named `emit`: Qt defines that as a macro, which swallows the call.
    inline int demuxAudioUnits(QByteArrayView sdu, const std::function<void(QByteArrayView)> &onUnit)
    {
        int units = 0;
        for (qsizetype off = kSduHeaderLen; off + kAuPrefixLen <= sdu.size();) {
            const qsizetype length = static_cast<quint8>(sdu[off + 4]);
            const qsizetype start = off + kAuPrefixLen;
            if (start + length > sdu.size()) {
                break;
            }
            onUnit(sdu.sliced(start, length));
            ++units;
            off = start + length;
        }
        return units;
    }
    // A pipe write can be partial, and s16 needs the stream on even byte offsets: a write that
    // stops mid-sample leaves the byte that completes it to lead the next one. Input can split a
    // sample too, as a read from parec does, so an odd last byte waits for the rest of its sample.
    // The sink returns the bytes it took, 0 for a full pipe; whole samples are what gets dropped.
    class SampleWriter
    {
    public:
        void write(QByteArrayView pcm, const std::function<qsizetype(QByteArrayView)> &sink)
        {
            QByteArray samples;
            samples.reserve(pcm.size() + 2);
            const bool carried = m_carry >= 0;
            if (carried) {
                samples.append(static_cast<char>(m_carry));
            }
            if (m_inputTail >= 0) {
                samples.append(static_cast<char>(m_inputTail));
            }
            samples.append(pcm.data(), pcm.size());
            m_inputTail = -1;
            if ((samples.size() - (carried ? 1 : 0)) % 2 == 1) {
                m_inputTail = static_cast<quint8>(samples.back());
                samples.chop(1);
            }
            if (samples.isEmpty()) {
                return;
            }
            const qsizetype written = qMax<qsizetype>(sink(samples), 0);
            const bool midSample = carried != (written % 2 == 1);
            m_carry = midSample ? static_cast<quint8>(samples[written]) : -1;
        }
        void reset()
        {
            m_carry = -1;
            m_inputTail = -1;
        }

    private:
        int m_carry = -1;
        int m_inputTail = -1;
    };
}
