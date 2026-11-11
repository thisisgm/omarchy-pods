#pragma once

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
}
