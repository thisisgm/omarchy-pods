// Two pairs on one laptop: the state of one must not show up as the state of the other.

#include <QTest>

#include "../pairidentity.hpp"

// main.cpp defines the Q_LOGGING_CATEGORY symbol, but tests don't link main.cpp.
Q_LOGGING_CATEGORY(openpods, "openpods.test", QtWarningMsg)

using AirpodsTrayApp::Enums::AirPodsModel;

class TestPairIdentity : public QObject
{
    Q_OBJECT

private slots:
    void stateBelongsTo_data()
    {
        QTest::addColumn<QString>("owner");
        QTest::addColumn<QString>("connecting");
        QTest::addColumn<bool>("belongs");
        QTest::newRow("same pair")        << "AA:BB:CC:00:00:01" << "AA:BB:CC:00:00:01" << true;
        QTest::newRow("case differs")     << "aa:bb:cc:00:00:01" << "AA:BB:CC:00:00:01" << true;
        QTest::newRow("other pair")       << "AA:BB:CC:00:00:02" << "AA:BB:CC:00:00:01" << false;
        // The regression: settings or BLE filled the state, no control link owns it.
        QTest::newRow("no owner")         << ""                  << "AA:BB:CC:00:00:01" << false;
    }

    void stateBelongsTo()
    {
        QFETCH(QString, owner);
        QFETCH(QString, connecting);
        QFETCH(bool, belongs);
        QCOMPARE(PairIdentity::stateBelongsTo(owner, connecting), belongs);
    }

    void advertIsFromOtherPair_data()
    {
        QTest::addColumn<bool>("connected");
        QTest::addColumn<int>("connectedModel");
        QTest::addColumn<int>("advertModel");
        QTest::addColumn<bool>("other");
        // The regression: a Pro 2 is connected and the Pro 3 in its case still resolves against the stored IRK.
        QTest::newRow("Pro 3 advert, Pro 2 connected")
            << true << int(AirPodsModel::AirPodsPro2USBC) << int(AirPodsModel::AirPodsPro3) << true;
        QTest::newRow("same model connected")
            << true << int(AirPodsModel::AirPodsPro3) << int(AirPodsModel::AirPodsPro3) << false;
        QTest::newRow("nothing connected")
            << false << int(AirPodsModel::AirPodsPro2USBC) << int(AirPodsModel::AirPodsPro3) << false;
        QTest::newRow("connected model unknown")
            << true << int(AirPodsModel::Unknown) << int(AirPodsModel::AirPodsPro3) << false;
        QTest::newRow("advert model unknown")
            << true << int(AirPodsModel::AirPodsPro2USBC) << int(AirPodsModel::Unknown) << false;
    }

    void advertIsFromOtherPair()
    {
        QFETCH(bool, connected);
        QFETCH(int, connectedModel);
        QFETCH(int, advertModel);
        QFETCH(bool, other);
        QCOMPARE(PairIdentity::advertIsFromOtherPair(connected,
                                                     static_cast<AirPodsModel>(connectedModel),
                                                     static_cast<AirPodsModel>(advertModel)),
                 other);
    }

    // The regression: a Pro 2 that sends no AAP metadata showed no model, or the last pair's.
    void adoptBluezIdentity_fillsWhatAapHasNotSent()
    {
        DeviceInfo info;
        QVERIFY(PairIdentity::adoptBluezIdentity(info, "Pods", "bluetooth:v004Cp2024d215C"));
        QCOMPARE(info.model(), AirPodsModel::AirPodsPro2USBC);
        QCOMPARE(info.deviceName(), QString("Pods"));
    }

    void adoptBluezIdentity_keepsWhatAapSent()
    {
        DeviceInfo info;
        info.setModel(AirPodsModel::AirPodsPro3);
        info.setDeviceName("From AAP");
        QVERIFY(!PairIdentity::adoptBluezIdentity(info, "From BlueZ", "bluetooth:v004Cp2024d215C"));
        QCOMPARE(info.model(), AirPodsModel::AirPodsPro3);
        QCOMPARE(info.deviceName(), QString("From AAP"));
    }

    void adoptBluezIdentity_unknownModaliasSetsNoModel()
    {
        DeviceInfo info;
        QVERIFY(!PairIdentity::adoptBluezIdentity(info, "Pods", ""));
        QCOMPARE(info.model(), AirPodsModel::Unknown);
        QCOMPARE(info.deviceName(), QString("Pods"));
    }
};

QTEST_GUILESS_MAIN(TestPairIdentity)
#include "tst_pairidentity.moc"
