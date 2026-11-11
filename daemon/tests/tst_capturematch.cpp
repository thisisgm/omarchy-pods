#include <QtTest>

#include "../media/capturematch.hpp"

// Source names in the shape PulseAudio publishes them, with a placeholder address.
class TestCaptureMatch : public QObject
{
    Q_OBJECT

private slots:
    void matchesTheWirePlumberLoopbackName();
    void matchesTheDirectNodeName();
    void rejectsAnotherDevice();
    void rejectsEmptyInput();
    void matchesTheMonitorNameToo();
    void anAnsweredQueryClearsTheUnansweredCount();
    void onlyAnUnknownQuerySpendsTheUnansweredCount();
    void onlyAnIdleCaptureReleasesTheCard();
};

void TestCaptureMatch::matchesTheWirePlumberLoopbackName()
{
    QVERIFY(sourceNamesAddress("bluez_input.AA:BB:CC:DD:EE:FF", "AA_BB_CC_DD_EE_FF"));
}

void TestCaptureMatch::matchesTheDirectNodeName()
{
    QVERIFY(sourceNamesAddress("bluez_input.AA_BB_CC_DD_EE_FF.0", "AA_BB_CC_DD_EE_FF"));
}

void TestCaptureMatch::rejectsAnotherDevice()
{
    QVERIFY(!sourceNamesAddress("bluez_input.11_22_33_44_55_66", "AA_BB_CC_DD_EE_FF"));
    QVERIFY(!sourceNamesAddress("alsa_input.pci-0000_00_1f.3.analog-stereo", "AA_BB_CC_DD_EE_FF"));
}

void TestCaptureMatch::rejectsEmptyInput()
{
    // Both empty is the load-bearing case: QString::contains("") is true without the guard.
    QVERIFY(!sourceNamesAddress("", ""));
    QVERIFY(!sourceNamesAddress("bluez_input.AA:BB:CC:DD:EE:FF", ""));
    QVERIFY(!sourceNamesAddress("", "AA_BB_CC_DD_EE_FF"));
}

void TestCaptureMatch::matchesTheMonitorNameToo()
{
    // A match means the address, not a capture, so the caller's monitor_of_sink filter is load-bearing.
    QVERIFY(sourceNamesAddress("bluez_output.AA_BB_CC_DD_EE_FF.1.monitor", "AA_BB_CC_DD_EE_FF"));
}

void TestCaptureMatch::onlyAnIdleCaptureReleasesTheCard()
{
    // A release while the mic is live takes the headset profiles with it, which ends the call.
    QVERIFY(!mayReleaseCard(CaptureState::Live));
    QVERIFY(!mayReleaseCard(CaptureState::Unknown));
    QVERIFY(mayReleaseCard(CaptureState::Idle));
}

void TestCaptureMatch::anAnsweredQueryClearsTheUnansweredCount()
{
    // Live and Idle are both answers, so neither may walk a long call towards the give-up path.
    QCOMPARE(unansweredChecksAfter(CaptureState::Live, 0), 0);
    QCOMPARE(unansweredChecksAfter(CaptureState::Live, 5), 0);
    QCOMPARE(unansweredChecksAfter(CaptureState::Idle, 5), 0);
}

void TestCaptureMatch::onlyAnUnknownQuerySpendsTheUnansweredCount()
{
    // Otherwise a PulseAudio that never answers re-checks forever instead of giving up.
    QCOMPARE(unansweredChecksAfter(CaptureState::Unknown, 0), 1);
    QCOMPARE(unansweredChecksAfter(CaptureState::Unknown, 5), 6);
}

QTEST_GUILESS_MAIN(TestCaptureMatch)
#include "tst_capturematch.moc"
