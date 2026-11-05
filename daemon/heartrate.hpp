#pragma once

#include <QByteArray>
#include <optional>

#include "BasicControlCommand.hpp"

// AirPods Pro 3 heart rate over AACP opcode 0x17 (SensorDataWX, a protobuf).
// Measured on an A3064 at firmware 90.3431: service 20 streams, a start on 19
// answers kIOReturnBadArgument, and the buds list their services (the heart rate
// one 921 bytes long) once per ACL, in reply to the host's first 0x17 message.
namespace HeartRate
{
    inline constexpr quint8 SERVICE = 0x14;
    inline constexpr quint8 HEAD_TRACKING_SERVICE = 0x0e;
    inline constexpr quint32 INTERVAL_US = 1000000;

    inline const QByteArray SENSOR_HEADER = QByteArray::fromHex("04000400170000001000");
    inline const QByteArray SERVICE_DESCRIPTOR = QByteArrayLiteral("HeartRateService");
    inline const QByteArray SAMPLE_MARKER = QByteArray::fromHex("3a1608141a12");
    inline constexpr int REPORT_SIZE = 18;

    inline const QByteArray SENSOR_ON = ControlCommand::createCommand(0x30, 0x01);
    inline const QByteArray SENSOR_OFF = ControlCommand::createCommand(0x30, 0x00);

    inline void appendVarint(QByteArray &out, quint32 value)
    {
        while (value >= 0x80) {
            out.append(static_cast<char>((value & 0x7f) | 0x80));
            value >>= 7;
        }
        out.append(static_cast<char>(value));
    }

    // seq is field 1 of the message; the buds accept any value but expect it to change.
    inline QByteArray streamPacket(quint8 service, quint32 seq, bool on, quint32 intervalUs = INTERVAL_US)
    {
        QByteArray body;
        body.append('\x08');
        appendVarint(body, seq);
        body.append(QByteArray::fromHex("420b08"));
        body.append(static_cast<char>(service));
        body.append(QByteArray::fromHex("10021a0501"));
        const quint32 interval = on ? intervalUs : 0;
        for (int i = 0; i < 4; ++i)
            body.append(static_cast<char>((interval >> (8 * i)) & 0xff));

        QByteArray packet = SENSOR_HEADER;
        packet.append(static_cast<char>(body.size() & 0xff));
        packet.append(static_cast<char>((body.size() >> 8) & 0xff));
        packet.append(body);
        return packet;
    }

    struct Sample
    {
        int bpm;
        // The first reports of a session carry 0x81 in the last byte and a bpm far off the real one.
        bool settled;
    };

    // Searches the whole buffer: a read can carry more than one AACP frame.
    inline std::optional<Sample> parseSample(const QByteArray &data)
    {
        const qsizetype at = data.lastIndexOf(SAMPLE_MARKER);
        if (at < 0 || data.size() < at + SAMPLE_MARKER.size() + REPORT_SIZE)
            return std::nullopt;
        const QByteArray report = data.mid(at + SAMPLE_MARKER.size(), REPORT_SIZE);
        if (static_cast<quint8>(report.at(0)) != 0x01)
            return std::nullopt;
        return Sample{static_cast<quint8>(report.at(1)), report.at(17) == 0};
    }
}
