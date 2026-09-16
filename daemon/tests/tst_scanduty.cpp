#include <QtTest>

#include "../scanduty.hpp"

class TestScanDuty : public QObject
{
    Q_OBJECT

private slots:
    void scanIsNotContinuous()
    {
        // The defect this replaced was setLowEnergyDiscoveryTimeout(0), which held the radio forever.
        QVERIFY(ScanDuty::scanWindowMs > 0);
        QVERIFY(ScanDuty::idleWindowMs > 0);

        // The accept-list scan only wins in the gap, so the gap is the larger half of the cycle.
        QVERIFY(ScanDuty::idleWindowMs > ScanDuty::scanWindowMs);
    }

    void resumeHoldOffLeavesTheRadioForHid()
    {
        // The previous 2s restart landed a discovery in the HID reconnect window.
        QVERIFY(ScanDuty::resumeHoldOffMs >= 10000);
        QVERIFY(ScanDuty::resumeHoldOffMs > ScanDuty::scanWindowMs);
    }

    void requestedStateSurvivesAWindowBoundary()
    {
        ScanDuty::Cycle cycle;
        cycle.request();
        QVERIFY(cycle.isRequested());

        QVERIFY(cycle.windowFinished());
        QCOMPARE(int(cycle.phase()), int(ScanDuty::Phase::Idle));

        // main.cpp saves this across control-link recovery, so a gap must not read as "scan off".
        QVERIFY(cycle.isRequested());

        QVERIFY(cycle.idleFinished());
        QCOMPARE(int(cycle.phase()), int(ScanDuty::Phase::Scanning));
        QVERIFY(cycle.isRequested());
    }

    void cancelDuringTheGapStaysDown()
    {
        ScanDuty::Cycle cycle;
        cycle.request();
        QVERIFY(cycle.windowFinished());
        cycle.cancel();
        QVERIFY(!cycle.isRequested());

        // The idle timer is already armed when stopScan lands, so its late tick must not resurrect the scan.
        QVERIFY(!cycle.idleFinished());
        QVERIFY(!cycle.isRequested());
    }

    void cancelDuringTheWindowStaysDown()
    {
        ScanDuty::Cycle cycle;
        cycle.request();
        cycle.cancel();
        QVERIFY(!cycle.isRequested());

        // stop() then emits canceled/finished, which must not open an idle gap for a scan nobody wants.
        QVERIFY(!cycle.windowFinished());
        QVERIFY(!cycle.isRequested());
    }

    void transitionsIgnoreTheWrongPhase()
    {
        ScanDuty::Cycle cycle;
        QVERIFY(!cycle.windowFinished());
        QVERIFY(!cycle.idleFinished());

        cycle.request();
        QVERIFY(!cycle.idleFinished());
        QVERIFY(cycle.windowFinished());

        // A second finished() for one window would otherwise arm the idle timer twice.
        QVERIFY(!cycle.windowFinished());
    }

    void requestDuringTheGapResumesImmediately()
    {
        ScanDuty::Cycle cycle;
        cycle.request();
        QVERIFY(cycle.windowFinished());

        cycle.request();
        QCOMPARE(int(cycle.phase()), int(ScanDuty::Phase::Scanning));
    }
};

QTEST_GUILESS_MAIN(TestScanDuty)
#include "tst_scanduty.moc"
