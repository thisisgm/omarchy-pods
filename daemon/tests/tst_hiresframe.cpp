#include <QtTest>

#include "../media/hiresframe.hpp"

using namespace OpenPods::HiRes;

// SDUs in the shape the AirPods send them: 04 00 04 00, message type 0x58, then the subtype.
static QByteArray sdu(quint16 subtype, const QList<QByteArray> &units, qsizetype headerLen = kSduHeaderLen)
{
    QByteArray out;
    out.append(QByteArray::fromHex("04000400"));
    out.append(static_cast<char>(0x58));
    out.append('\0');
    out.append(static_cast<char>(subtype & 0xff));
    out.append(static_cast<char>(subtype >> 8));
    out.append(QByteArray(headerLen - out.size(), '\0'));
    for (const QByteArray &unit : units) {
        out.append(QByteArray(4, '\0'));  // timestamp
        out.append(static_cast<char>(unit.size()));
        out.append(unit);
    }
    return out;
}

class TestHiResFrame : public QObject
{
    Q_OBJECT

private slots:
    void acceptsAnAudioSdu();
    void rejectsTheControlSubtype();
    void rejectsShortAndForeignFrames();
    void demuxesEveryAccessUnit();
    void stopsOnATruncatedAccessUnit();
    void emitsNothingFromAHeaderOnlySdu();
};

void TestHiResFrame::acceptsAnAudioSdu()
{
    QVERIFY(isAudioSdu(sdu(0x0001, {QByteArray(8, 'a')})));
}

void TestHiResFrame::rejectsTheControlSubtype()
{
    // The start and stop packets are type 0x58 too, so only the subtype keeps them out of the decoder.
    QVERIFY(!isAudioSdu(QByteArray::fromHex("040004005800000009000001")));
    QVERIFY(!isAudioSdu(sdu(0x0000, {QByteArray(8, 'a')})));
    QVERIFY(!isAudioSdu(sdu(0x0003, {QByteArray(8, 'a')})));
}

void TestHiResFrame::rejectsShortAndForeignFrames()
{
    QVERIFY(!isAudioSdu(QByteArray::fromHex("0400040058")));           // shorter than the subtype
    QVERIFY(!isAudioSdu(QByteArray()));
    QVERIFY(!isAudioSdu(QByteArray::fromHex("040004000e0001000000"))); // battery, another message type
}

void TestHiResFrame::demuxesEveryAccessUnit()
{
    const QByteArray first(40, '\x11');
    const QByteArray second(37, '\x22');
    QList<QByteArray> seen;
    const int count = demuxAudioUnits(sdu(0x0001, {first, second}),
                                      [&seen](QByteArrayView au) { seen.append(au.toByteArray()); });
    QCOMPARE(count, 2);
    QCOMPARE(seen, QList<QByteArray>({first, second}));
}

void TestHiResFrame::stopsOnATruncatedAccessUnit()
{
    // A 4096-byte read buffer can still cut an SDU, and half an access unit is not decodable.
    QByteArray frame = sdu(0x0001, {QByteArray(40, '\x11'), QByteArray(40, '\x22')});
    frame.chop(12);
    int count = 0;
    count = demuxAudioUnits(frame, [&count](QByteArrayView) { });
    QCOMPARE(count, 1);
}

void TestHiResFrame::emitsNothingFromAHeaderOnlySdu()
{
    QCOMPARE(demuxAudioUnits(sdu(0x0001, {}), [](QByteArrayView) { }), 0);
}

QTEST_GUILESS_MAIN(TestHiResFrame)
#include "tst_hiresframe.moc"
