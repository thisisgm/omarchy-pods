// Frames captured from an A3064 at firmware 90.3431 on 2026-11-05.

#include "heartrate.hpp"

#include <QtTest/QtTest>

class TestHeartRate : public QObject
{
    Q_OBJECT

private slots:
    void startPacket_matchesWhatTheBudsAcknowledged()
    {
        QCOMPARE(HeartRate::streamPacket(HeartRate::SERVICE, 1, true),
                 QByteArray::fromHex("040004001700000010000f000801420b081410021a050140420f00"));
    }

    void stopPacket_zeroesTheInterval()
    {
        QCOMPARE(HeartRate::streamPacket(HeartRate::SERVICE, 2, false),
                 QByteArray::fromHex("040004001700000010000f000802420b081410021a050100000000"));
    }

    void seqPastOneByte_isAVarint()
    {
        const QByteArray packet = HeartRate::streamPacket(HeartRate::SERVICE, 300, true);
        QCOMPARE(packet.mid(10, 2), QByteArray::fromHex("1000"));
        QCOMPARE(packet.mid(12, 3), QByteArray::fromHex("08ac02"));
    }

    void headTrackingStop_matchesTheProbe()
    {
        QCOMPARE(HeartRate::streamPacket(HeartRate::HEAD_TRACKING_SERVICE, 134, false),
                 QByteArray::fromHex("040004001700000010001000088601420b080e10021a050100000000"));
    }

    void settledSample_parses()
    {
        const auto sample = HeartRate::parseSample(QByteArray::fromHex(
            "040004001700000010001d0008c5071001"
            "3a1608141a12013aca070002e873b1d1710e000000100000"));
        QVERIFY(sample.has_value());
        QCOMPARE(sample->bpm, 58);
        QVERIFY(sample->settled);
    }

    void warmUpSample_isNotSettled()
    {
        const auto sample = HeartRate::parseSample(QByteArray::fromHex(
            "040004001700000010001d0008be071001"
            "3a1608141a1201a914000002e8e77630700e000001108281"));
        QVERIFY(sample.has_value());
        QCOMPARE(sample->bpm, 169);
        QVERIFY(!sample->settled);
    }

    void sampleAfterAnotherFrame_isFound()
    {
        const QByteArray two = QByteArray::fromHex("0400040006000000")
            + QByteArray::fromHex("040004001700000010001d0008c5071001"
                                  "3a1608141a12013aca070002e873b1d1710e000000100000");
        QCOMPARE(HeartRate::parseSample(two).value_or(HeartRate::Sample{0, false}).bpm, 58);
    }

    void truncatedSample_isRejected()
    {
        QVERIFY(!HeartRate::parseSample(QByteArray::fromHex("3a1608141a12013aca07")).has_value());
    }

    void serviceListWithoutSample_isRejected()
    {
        QVERIFY(!HeartRate::parseSample(QByteArray::fromHex(
            "040004001700000010000d0008bb0710016202081362020814")).has_value());
    }
};

QTEST_GUILESS_MAIN(TestHeartRate)
#include "tst_heartrate.moc"
