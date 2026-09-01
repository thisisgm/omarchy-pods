#include <QtTest>

#include "../scanduty.hpp"

class TestScanDuty : public QObject
{
    Q_OBJECT

private slots:
    void yieldsTheRadioBetweenWindows()
    {
        // The defect this replaces: the agent ran with a 0ms timeout, so the scan never
        // ended and a reconnecting BLE HID device never got the radio.
        ScanDuty::Cycle cycle;
        cycle.start();
        QCOMPARE(cycle.phase(), ScanDuty::Phase::Window);

        QVERIFY(cycle.windowFinished());
        QCOMPARE(cycle.phase(), ScanDuty::Phase::Gap);
        QVERIFY(cycle.active());

        QVERIFY(cycle.gapFinished());
        QCOMPARE(cycle.phase(), ScanDuty::Phase::Window);
    }

    void leavesTheRadioIdleForAtLeastHalfTheCycle()
    {
        QVERIFY(ScanDuty::gapMs >= ScanDuty::windowMs);
        QVERIFY(ScanDuty::resumeSettleMs > ScanDuty::gapMs);
    }

    void staysStoppedWhenTheAgentReportsFinishedAfterAStop()
    {
        // stopScan() cancels the agent, and the cancellation arrives as a signal. Treating
        // it as the end of a window would restart a scan the caller just switched off.
        ScanDuty::Cycle cycle;
        cycle.start();
        cycle.stop();

        QVERIFY(!cycle.windowFinished());
        QVERIFY(!cycle.active());
        QCOMPARE(cycle.phase(), ScanDuty::Phase::Idle);
    }

    void staysStoppedWhenAGapTimerFiresAfterAStop()
    {
        ScanDuty::Cycle cycle;
        cycle.start();
        QVERIFY(cycle.windowFinished());
        cycle.stop();

        QVERIFY(!cycle.gapFinished());
        QVERIFY(!cycle.active());
    }

    void ignoresAGapThatEndsTwice()
    {
        ScanDuty::Cycle cycle;
        cycle.start();
        QVERIFY(cycle.windowFinished());
        QVERIFY(cycle.gapFinished());

        // Already scanning again, so a duplicate timeout must not re-arm anything.
        QVERIFY(!cycle.gapFinished());
        QCOMPARE(cycle.phase(), ScanDuty::Phase::Window);
    }

    void startsIdle()
    {
        ScanDuty::Cycle cycle;
        QVERIFY(!cycle.active());
        QVERIFY(!cycle.windowFinished());
        QVERIFY(!cycle.gapFinished());
    }
};

QTEST_GUILESS_MAIN(TestScanDuty)
#include "tst_scanduty.moc"
