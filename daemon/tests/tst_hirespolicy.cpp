#include <QtTest>

#include "media/hirespolicy.hpp"

using namespace OpenPods::HiRes;

class TestHiResPolicy : public QObject
{
    Q_OBJECT

private slots:
    void startsOnlyWhenEnabledRecordingAndWorn()
    {
        QCOMPARE(captureAction(true, true, true, false, false), CaptureAction::Start);
        QCOMPARE(captureAction(false, true, true, false, false), CaptureAction::Idle);
        QCOMPARE(captureAction(true, false, true, false, false), CaptureAction::Idle);
        QCOMPARE(captureAction(true, true, false, false, false), CaptureAction::Idle);
    }

    void budsOutStopsAndNeverRestarts()
    {
        QCOMPARE(captureAction(true, true, false, true, true), CaptureAction::Stop);
        QCOMPARE(captureAction(true, true, false, true, false), CaptureAction::Stop);
    }

    void closingTheMicOrDisablingStops()
    {
        QCOMPARE(captureAction(true, false, true, true, false), CaptureAction::Stop);
        QCOMPARE(captureAction(false, true, true, true, false), CaptureAction::Stop);
    }

    void stallRestartsOnlyWhileCapturingAndWorn()
    {
        QCOMPARE(captureAction(true, true, true, true, true), CaptureAction::Restart);
        QCOMPARE(captureAction(true, true, true, true, false), CaptureAction::Keep);
    }

    void backoffDoublesUpToTheCap()
    {
        QCOMPARE(nextBackoffMs(kStallMs), 4000);
        QCOMPARE(nextBackoffMs(16000), kMaxBackoffMs);
        QCOMPARE(nextBackoffMs(kMaxBackoffMs), kMaxBackoffMs);
    }

    void parkedStateRoundTrips()
    {
        const QString mic = QStringLiteral("alsa_input.usb-mic.mono-fallback");
        QCOMPARE(parseParked(serializeParked(mic)), mic);
        QCOMPARE(serializeParked(QString()), QString());
        QCOMPARE(parseParked(QString()), QString());
        QCOMPARE(parseParked(QStringLiteral("garbage\nmoved\t7\tapp\n")), QString());
    }
};

QTEST_GUILESS_MAIN(TestHiResPolicy)
#include "tst_hirespolicy.moc"
