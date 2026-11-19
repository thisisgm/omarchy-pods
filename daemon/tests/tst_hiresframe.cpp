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
    void keepsSamplesWholeAcrossShortWrites();
    void dropsWholeSamplesWhenThePipeIsFull();
    void joinsOddInputChunksWrittenInFull();
    void joinsOddInputChunksAcrossShortWrites();
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

void TestHiResFrame::keepsSamplesWholeAcrossShortWrites()
{
    OpenPods::HiRes::SampleWriter writer;
    QByteArray pipe;
    // Odd short writes, as a nonblocking pipe can return for a write over PIPE_BUF.
    const QList<qsizetype> takes {3, 5, 1, 5};
    int call = 0;
    auto sink = [&](QByteArrayView bytes) {
        const qsizetype n = qMin(takes[call++], bytes.size());
        pipe.append(bytes.first(n));
        return n;
    };
    writer.write(QByteArray::fromHex("0102030405060708"), sink);
    writer.write(QByteArray::fromHex("1112131415161718"), sink);
    writer.write(QByteArray::fromHex("2122"), sink);
    writer.write(QByteArray::fromHex("31323334"), sink);
    QCOMPARE(pipe.size() % 2, 0);
    // Every sample in the pipe is one that was sent, never the tail of one and the head of the next.
    const QList<QByteArray> sent {"\x01\x02", "\x03\x04", "\x05\x06", "\x07\x08", "\x11\x12", "\x13\x14",
                                  "\x15\x16", "\x17\x18", "\x21\x22", "\x31\x32", "\x33\x34"};
    for (qsizetype i = 0; i < pipe.size(); i += 2) {
        QVERIFY2(sent.contains(pipe.mid(i, 2)), qPrintable(pipe.mid(i, 2).toHex()));
    }
}

void TestHiResFrame::dropsWholeSamplesWhenThePipeIsFull()
{
    OpenPods::HiRes::SampleWriter writer;
    QByteArray pipe;
    qsizetype room = 1;
    auto sink = [&](QByteArrayView bytes) {
        const qsizetype n = qMin(room, bytes.size());
        pipe.append(bytes.first(n));
        room -= n;
        return n;
    };
    writer.write(QByteArray::fromHex("0a0b0c0d"), sink);
    writer.write(QByteArray::fromHex("1a1b"), sink);
    room = 64;
    writer.write(QByteArray::fromHex("2a2b"), sink);
    QCOMPARE(pipe, QByteArray::fromHex("0a0b2a2b"));
}

void TestHiResFrame::joinsOddInputChunksWrittenInFull()
{
    OpenPods::HiRes::SampleWriter writer;
    QByteArray pipe;
    auto sink = [&](QByteArrayView bytes) {
        pipe.append(bytes);
        return bytes.size();
    };
    // A read from parec can end anywhere, one byte included.
    writer.write(QByteArray::fromHex("01"), sink);
    writer.write(QByteArray::fromHex("0203"), sink);
    writer.write(QByteArray::fromHex("04"), sink);
    writer.write(QByteArray::fromHex("050607"), sink);
    writer.write(QByteArray::fromHex("08"), sink);
    QCOMPARE(pipe, QByteArray::fromHex("0102030405060708"));
}

void TestHiResFrame::joinsOddInputChunksAcrossShortWrites()
{
    OpenPods::HiRes::SampleWriter writer;
    QByteArray pipe;
    const QList<qsizetype> takes {1, 0, 64, 64};
    int call = 0;
    auto sink = [&](QByteArrayView bytes) {
        const qsizetype n = qMin(takes[call++], bytes.size());
        pipe.append(bytes.first(n));
        return n;
    };
    writer.write(QByteArray::fromHex("010203"), sink);
    writer.write(QByteArray::fromHex("0405"), sink);
    writer.write(QByteArray::fromHex("060708"), sink);
    writer.write(QByteArray::fromHex("090a0b"), sink);
    // Only 0304 fell on the full pipe; 02 still completes its sample, so every sample left is whole and in order.
    QCOMPARE(pipe, QByteArray::fromHex("010205060708090a"));
    writer.write(QByteArray::fromHex("0c"), [&](QByteArrayView bytes) {
        pipe.append(bytes);
        return bytes.size();
    });
    QCOMPARE(pipe, QByteArray::fromHex("010205060708090a0b0c"));
}

QTEST_GUILESS_MAIN(TestHiResFrame)
#include "tst_hiresframe.moc"
