// Frames captured from an A3064 at firmware 90.3431 on 2026-11-05.

#include "motion.hpp"

#include <QtTest/QtTest>

#include <cmath>

class TestMotion : public QObject
{
    Q_OBJECT

private slots:
    void warmUpFrame_isDropped()
    {
        QVERIFY(!Motion::parseMotion(QByteArray::fromHex(
            "04000400170000001000450008d10310013a3e08101a3a016068b1ad661700000300821087510d00000000000000000000000000000000621003e93404a1026102c005a2026802c201827fde38be0b0000")));
    }

    void streamingFrame_parses()
    {
        const auto s = Motion::parseMotion(QByteArray::fromHex(
            "04000400170000001000450008f70310013a3e08101a3a0118e1df07671700000300433f9d680d0000000086953d177026d4ff91ffc0ff610003e93404fffff3ff030099026302d30160d26638ee0c0000"));
        QVERIFY(s.has_value());
        QVERIFY(std::abs(s->x - -0.8318f) < 1e-4f);
        QVERIFY(std::abs(s->y - 0.1815f) < 1e-4f);
        QVERIFY(std::abs(s->z - 0.3003f) < 1e-4f);
        QVERIFY(std::abs(s->w - std::sqrt(1.0f - s->x * s->x - s->y * s->y - s->z * s->z)) < 1e-6f);
        QCOMPARE(s->gravity, (std::array<int, 3>{665, 611, 467}));
    }

    void shortFrame_isDropped()
    {
        QVERIFY(!Motion::parseMotion(QByteArray::fromHex("04000400170000001000450008f70310013a3e08101a3a0118e1df")));
    }

    void startPacket_carriesTheMotionInterval()
    {
        const QByteArray packet = HeartRate::streamPacket(Motion::SERVICE, 1, true, Motion::INTERVAL_US);
        QVERIFY(packet.endsWith(QByteArray::fromHex("409c0000")));
    }
};

QTEST_GUILESS_MAIN(TestMotion)
#include "tst_motion.moc"
