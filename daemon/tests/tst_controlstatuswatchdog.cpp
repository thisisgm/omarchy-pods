// The control status watchdog: what it asks for when a new control link stays silent or sends no battery.

#include <QSignalSpy>
#include <QTest>

#include "../controlstatuswatchdog.hpp"

// main.cpp defines the Q_LOGGING_CATEGORY symbol, but tests don't link main.cpp.
Q_LOGGING_CATEGORY(openpods, "openpods.test", QtWarningMsg)

// Short enough to run the whole ladder in a test, long enough for the event loop to order the timeouts.
static constexpr int testWaitMs = 10;
// Every step the watchdog can take, with room to spare, so a missing signal fails instead of hanging.
static constexpr int ladderSettleMs = 40 * testWaitMs;

class TestControlStatusWatchdog : public QObject
{
    Q_OBJECT

private slots:
    void silentLinkResendsThenReopens()
    {
        ControlStatusWatchdog watchdog(testWaitMs);
        QSignalSpy resends(&watchdog, &ControlStatusWatchdog::resendHandshake);
        QSignalSpy reopens(&watchdog, &ControlStatusWatchdog::reopenSocket);
        QSignalSpy requests(&watchdog, &ControlStatusWatchdog::requestNotifications);

        watchdog.linkOpened();

        QTRY_COMPARE_WITH_TIMEOUT(reopens.count(), 1, ladderSettleMs);
        QCOMPARE(resends.count(), ControlReconnect::statusHandshakeResendLimit);
        QCOMPARE(requests.count(), 0);
    }

    // The regression: after the handshakes the daemon stopped, and the link stayed dead until a restart.
    void reopensStopAtTheLimitPerBluezConnection()
    {
        ControlStatusWatchdog watchdog(testWaitMs);
        QSignalSpy reopens(&watchdog, &ControlStatusWatchdog::reopenSocket);

        for (int link = 0; link <= ControlReconnect::silentLinkReopenLimit; ++link) {
            watchdog.linkOpened();
            QTest::qWait(ladderSettleMs);
        }
        QCOMPARE(reopens.count(), ControlReconnect::silentLinkReopenLimit);

        watchdog.bluezConnected();
        watchdog.linkOpened();
        QTRY_COMPARE_WITH_TIMEOUT(reopens.count(), ControlReconnect::silentLinkReopenLimit + 1, ladderSettleMs);
    }

    // The regression: ear state arrived, battery did not, and nothing asked again until a level changed.
    void earWithoutBatteryRequestsNotificationsAgain()
    {
        ControlStatusWatchdog watchdog(testWaitMs);
        QSignalSpy resends(&watchdog, &ControlStatusWatchdog::resendHandshake);
        QSignalSpy requests(&watchdog, &ControlStatusWatchdog::requestNotifications);

        watchdog.linkOpened();
        watchdog.earStatusReceived();
        QCOMPARE(watchdog.status(), ControlStatusWatchdog::LinkStatus::EarOnly);

        QTest::qWait(ladderSettleMs);
        QCOMPARE(requests.count(), ControlReconnect::batteryRequestLimit);
        QCOMPARE(resends.count(), 0);
    }

    void batteryStopsTheWatchdog()
    {
        ControlStatusWatchdog watchdog(testWaitMs);
        QSignalSpy resends(&watchdog, &ControlStatusWatchdog::resendHandshake);
        QSignalSpy requests(&watchdog, &ControlStatusWatchdog::requestNotifications);

        watchdog.linkOpened();
        watchdog.earStatusReceived();
        watchdog.batteryReceived();
        QCOMPARE(watchdog.status(), ControlStatusWatchdog::LinkStatus::BatterySeen);

        QTest::qWait(ladderSettleMs);
        QCOMPARE(requests.count(), 0);
        QCOMPARE(resends.count(), 0);
    }

    void closedLinkAsksForNothing()
    {
        ControlStatusWatchdog watchdog(testWaitMs);
        QSignalSpy resends(&watchdog, &ControlStatusWatchdog::resendHandshake);

        watchdog.linkOpened();
        watchdog.linkClosed();

        QTest::qWait(ladderSettleMs);
        QCOMPARE(resends.count(), 0);
    }

    // A late ear report must not take a link back from battery to ear only.
    void earAfterBatteryKeepsBattery()
    {
        ControlStatusWatchdog watchdog(testWaitMs);

        watchdog.linkOpened();
        watchdog.batteryReceived();
        watchdog.earStatusReceived();

        QCOMPARE(watchdog.status(), ControlStatusWatchdog::LinkStatus::BatterySeen);
    }
};

QTEST_GUILESS_MAIN(TestControlStatusWatchdog)
#include "tst_controlstatuswatchdog.moc"
