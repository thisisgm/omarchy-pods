#include <QtTest>

#include "../media/agc.hpp"
#include "../media/eldpcm.hpp"

using namespace OpenPods::HiRes;

// A frame of the uplink's shape: 480 mono samples of a steady tone at a given amplitude.
static QList<qint16> tone(double amplitude, int count = kEldFrameSamples)
{
    QList<qint16> out;
    out.reserve(count);
    for (int i = 0; i < count; ++i) {
        out.append(static_cast<qint16>(std::sin(i * 0.05) * amplitude * 32767.0));
    }
    return out;
}

static double rms(const QList<qint16> &samples)
{
    double sum = 0.0;
    for (qint16 s : samples) {
        sum += double(s) * double(s);
    }
    return std::sqrt(sum / samples.size());
}

class TestPreamp : public QObject
{
    Q_OBJECT

private slots:
    void liftsAVoiceThatSitsNearTheRoomLevel();
    void leavesSilenceSilent();
    void keepsLoudInputInRange();
    void preservesTheGapBetweenAVoiceAndTheRoom();
};

void TestPreamp::liftsAVoiceThatSitsNearTheRoomLevel()
{
    // Measured on the uplink: a voice arrives near -51 dBFS and must reach the level the
    // suppressor downstream was trained on.
    QList<qint16> voice = tone(0.003);
    const double before = rms(voice);
    Preamp preamp;
    preamp.process(voice.data(), voice.size());
    // Tied to the constant rather than a copy of it, so retuning the preamp does not quietly
    // invalidate this. The limiter costs a little, hence the margin.
    QVERIFY2(rms(voice) > before * Preamp::gain() * 0.8,
             qPrintable(QStringLiteral("a quiet voice went from %1 to %2, gain is %3")
                            .arg(before).arg(rms(voice)).arg(Preamp::gain())));
}

void TestPreamp::leavesSilenceSilent()
{
    QList<qint16> silence(kEldFrameSamples, 0);
    Preamp preamp;
    preamp.process(silence.data(), silence.size());
    for (qint16 s : silence) {
        QCOMPARE(s, qint16(0));
    }
}

void TestPreamp::keepsLoudInputInRange()
{
    // Without the limiter a gained transient wraps sign, which is audible as a click.
    QList<qint16> full = tone(0.99);
    Preamp preamp;
    preamp.process(full.data(), full.size());
    for (qint16 s : full) {
        QVERIFY(s >= -32767 && s <= 32767);
    }
}

void TestPreamp::preservesTheGapBetweenAVoiceAndTheRoom()
{
    // A tracked gain closed this gap to about 2 dB, which left the suppressor unable to tell a
    // voice from the fans. A fixed gain must not.
    QList<qint16> voice = tone(0.003);
    QList<qint16> room = tone(0.001);
    const double gapBefore = 20 * std::log10(rms(voice) / rms(room));
    Preamp preamp;
    preamp.process(voice.data(), voice.size());
    Preamp other;
    other.process(room.data(), room.size());
    const double gapAfter = 20 * std::log10(rms(voice) / rms(room));
    QVERIFY2(std::fabs(gapAfter - gapBefore) < 1.0,
             qPrintable(QStringLiteral("gap moved from %1 dB to %2 dB").arg(gapBefore).arg(gapAfter)));
}

QTEST_GUILESS_MAIN(TestPreamp)
#include "tst_agc.moc"
