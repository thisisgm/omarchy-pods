#pragma once

#include <QByteArray>
#include <QtGlobal>
#include <array>
#include <cmath>
#include <optional>

#include "heartrate.hpp"

// AirPods Pro 3 head motion: sensor service 16 ("devmotion6"), streamed with the heart rate's
// packet shape at 25 Hz. Measured on an A3064 at firmware 90.3431. Each report carries the vector
// part of a unit quaternion, so w is rebuilt; yaw is relative to where the stream started.
namespace Motion
{
    inline constexpr quint8 SERVICE = 0x10;
    inline constexpr quint32 INTERVAL_US = 40000;

    inline const QByteArray SERVICE_DESCRIPTOR = QByteArrayLiteral("devmotion");
    inline const QByteArray SAMPLE_MARKER = QByteArray::fromHex("3a3e08101a3a");
    inline constexpr int REPORT_SIZE = 58;

    struct Sample
    {
        float w, x, y, z;
        std::array<int, 3> gravity;  // milli-g
        std::array<int, 3> gyro;     // raw units
        quint32 counter;
    };

    inline int i16(const QByteArray &report, int at)
    {
        return static_cast<qint16>(static_cast<quint8>(report.at(at)) | (static_cast<quint8>(report.at(at + 1)) << 8));
    }

    // Searches the whole buffer: a read can carry more than one AACP frame.
    // The first frames of a stream carry a zero quaternion, which is dropped.
    inline std::optional<Sample> parseMotion(const QByteArray &data)
    {
        const qsizetype at = data.lastIndexOf(SAMPLE_MARKER);
        if (at < 0 || data.size() < at + SAMPLE_MARKER.size() + REPORT_SIZE)
            return std::nullopt;
        const QByteArray r = data.mid(at + SAMPLE_MARKER.size(), REPORT_SIZE);
        if (static_cast<quint8>(r.at(0)) != 0x01)
            return std::nullopt;
        const int qx = i16(r, 20), qy = i16(r, 22), qz = i16(r, 24);
        if (qx == 0 && qy == 0 && qz == 0)
            return std::nullopt;
        Sample s;
        s.x = qx / 32768.0f;
        s.y = qy / 32768.0f;
        s.z = qz / 32768.0f;
        s.w = std::sqrt(std::fmax(0.0f, 1.0f - s.x * s.x - s.y * s.y - s.z * s.z));
        s.gyro = {i16(r, 26), i16(r, 28), i16(r, 30)};
        s.gravity = {i16(r, 44), i16(r, 46), i16(r, 48)};
        s.counter = static_cast<quint8>(r.at(54)) | (static_cast<quint8>(r.at(55)) << 8)
            | (static_cast<quint8>(r.at(56)) << 16) | (static_cast<quint32>(static_cast<quint8>(r.at(57))) << 24);
        return s;
    }
}
