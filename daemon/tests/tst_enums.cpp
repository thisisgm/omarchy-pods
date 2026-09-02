// Regression test for enums.h getModelIcon — every returned filename
// must exist under linux/assets/. Pre-fix, AirPods Max referenced
// "max_case.png" which doesn't ship; broken Image source at runtime.
//
// Also pins parseModelNumber against the public Apple support
// reference (A1523 / A2032 / A2096 / A3047 / A3053 etc.).

#include <QTest>
#include <QString>
#include <QDir>
#include <QFile>

#include "../enums.h"

using namespace AirpodsTrayApp::Enums;

class TestEnums : public QObject
{
    Q_OBJECT

private slots:
    void initTestCase()
    {
        m_assetsDir = QStringLiteral(OPENPODS_ASSETS_DIR);
        QVERIFY2(QDir(m_assetsDir).exists(), qPrintable("assets dir missing: " + m_assetsDir));
    }

    void allModelIconsExist_data()
    {
        QTest::addColumn<int>("model");
        QTest::newRow("AirPods1")          << int(AirPodsModel::AirPods1);
        QTest::newRow("AirPods2")          << int(AirPodsModel::AirPods2);
        QTest::newRow("AirPods3")          << int(AirPodsModel::AirPods3);
        QTest::newRow("AirPods4")          << int(AirPodsModel::AirPods4);
        QTest::newRow("AirPods4ANC")       << int(AirPodsModel::AirPods4ANC);
        QTest::newRow("AirPodsPro")        << int(AirPodsModel::AirPodsPro);
        QTest::newRow("AirPodsPro2L")      << int(AirPodsModel::AirPodsPro2Lightning);
        QTest::newRow("AirPodsPro2USBC")   << int(AirPodsModel::AirPodsPro2USBC);
        QTest::newRow("AirPodsMaxL")       << int(AirPodsModel::AirPodsMaxLightning);
        QTest::newRow("AirPodsMaxUSBC")    << int(AirPodsModel::AirPodsMaxUSBC);
        QTest::newRow("AirPodsMax2")       << int(AirPodsModel::AirPodsMax2);
        QTest::newRow("PowerbeatsPro")     << int(AirPodsModel::PowerbeatsPro);
        QTest::newRow("Unknown")           << int(AirPodsModel::Unknown);
    }

    void allModelIconsExist()
    {
        QFETCH(int, model);
        auto icons = getModelIcon(static_cast<AirPodsModel>(model));
        const QString podPath  = m_assetsDir + "/" + icons.first;
        const QString casePath = m_assetsDir + "/" + icons.second;
        QVERIFY2(QFile::exists(podPath),
                 qPrintable("pod icon missing: " + podPath));
        QVERIFY2(QFile::exists(casePath),
                 qPrintable("case icon missing: " + casePath));
    }

    void parseModelNumber_matchesAppleSupportList()
    {
        QCOMPARE(parseModelNumber("A1523"), AirPodsModel::AirPods1);
        QCOMPARE(parseModelNumber("A2032"), AirPodsModel::AirPods2);
        QCOMPARE(parseModelNumber("A2096"), AirPodsModel::AirPodsMaxLightning);
        QCOMPARE(parseModelNumber("A3184"), AirPodsModel::AirPodsMaxUSBC);
        QCOMPARE(parseModelNumber("A3454"), AirPodsModel::AirPodsMax2);
        QCOMPARE(parseModelNumber("A2565"), AirPodsModel::AirPods3);
        QCOMPARE(parseModelNumber("A3047"), AirPodsModel::AirPodsPro2USBC);
        QCOMPARE(parseModelNumber("A2931"), AirPodsModel::AirPodsPro2Lightning);
        QCOMPARE(parseModelNumber("A3053"), AirPodsModel::AirPods4);
        QCOMPARE(parseModelNumber("A3056"), AirPodsModel::AirPods4ANC);
        QCOMPARE(parseModelNumber("A3064"), AirPodsModel::AirPodsPro3);
        QCOMPARE(parseModelNumber("A3063"), AirPodsModel::AirPodsPro3);
        QCOMPARE(parseModelNumber("A3065"), AirPodsModel::AirPodsPro3);
        QCOMPARE(parseModelNumber("A2047"), AirPodsModel::PowerbeatsPro);
        QCOMPARE(parseModelNumber("A2048"), AirPodsModel::PowerbeatsPro);
        QCOMPARE(parseModelNumber("A2078"), AirPodsModel::PowerbeatsPro);
        QCOMPARE(parseModelNumber("A3157"), AirPodsModel::PowerbeatsPro);
        // Apple publishes none of these four, so the dropped guesses must not answer Pro 3.
        QCOMPARE(parseModelNumber("A3066"), AirPodsModel::Unknown);
        QCOMPARE(parseModelNumber("A3334"), AirPodsModel::Unknown);
        QCOMPARE(parseModelNumber("A3335"), AirPodsModel::Unknown);
        QCOMPARE(parseModelNumber("A3336"), AirPodsModel::Unknown);
        QCOMPARE(parseModelNumber("ZZZZZ"), AirPodsModel::Unknown);
        QCOMPARE(parseModelNumber(""),      AirPodsModel::Unknown);
    }

    void modelDisplayName_covers_all() {
        QCOMPARE(modelDisplayName(AirPodsModel::AirPodsPro3),
                 QStringLiteral("AirPods Pro 3"));
        QCOMPARE(modelDisplayName(AirPodsModel::AirPodsPro2USBC),
                 QStringLiteral("AirPods Pro 2 (USB-C)"));
        QCOMPARE(modelDisplayName(AirPodsModel::PowerbeatsPro),
                 QStringLiteral("Powerbeats Pro"));
        QCOMPARE(modelDisplayName(AirPodsModel::Unknown), QString());
    }

    // Exhaustive guard: every enum value the daemon can reach via
    // parseModelNumber must produce a non-empty user-facing string
    // (except Unknown which is intentionally empty). Catches future
    // enum additions where the contributor adds the map entry but
    // forgets to extend modelDisplayName's switch. Walks the full
    // enum range explicitly rather than via reflection because
    // AirPodsModel isn't a Q_ENUM.
    void modelDisplayName_exhaustive() {
        const AirPodsModel known[] = {
            AirPodsModel::AirPods1,
            AirPodsModel::AirPods2,
            AirPodsModel::AirPods3,
            AirPodsModel::AirPods4,
            AirPodsModel::AirPods4ANC,
            AirPodsModel::AirPodsPro,
            AirPodsModel::AirPodsPro2Lightning,
            AirPodsModel::AirPodsPro2USBC,
            AirPodsModel::AirPodsPro3,
            AirPodsModel::AirPodsMaxLightning,
            AirPodsModel::AirPodsMaxUSBC,
            AirPodsModel::AirPodsMax2,
            AirPodsModel::PowerbeatsPro,
        };
        for (const auto m : known) {
            const QString name = modelDisplayName(m);
            QVERIFY2(!name.isEmpty(),
                     qPrintable(QStringLiteral("modelDisplayName(%1) is empty — enum addition missing switch case")
                                .arg(static_cast<int>(m))));
        }
        // Unknown must stay empty so consumers can skip-render.
        QCOMPARE(modelDisplayName(AirPodsModel::Unknown), QString());
        // The loop above only asks for a non-empty string, so pin the published names too.
        QCOMPARE(modelDisplayName(AirPodsModel::AirPodsMax2), QStringLiteral("AirPods Max 2"));
        QCOMPARE(modelDisplayName(AirPodsModel::PowerbeatsPro), QStringLiteral("Powerbeats Pro"));
        // Both AirPods 4 variants say AirPods 4 on purpose, so a reviewer cannot read the match as a slip.
        QCOMPARE(modelDisplayName(AirPodsModel::AirPods4), QStringLiteral("AirPods 4"));
        QCOMPARE(modelDisplayName(AirPodsModel::AirPods4ANC), QStringLiteral("AirPods 4"));
    }

    // model_int is persisted and published, so any renumbering here is a break, not a refactor.
    void modelIntsAreStable()
    {
        QCOMPARE(int(AirPodsModel::Unknown), 0);
        QCOMPARE(int(AirPodsModel::AirPods1), 1);
        QCOMPARE(int(AirPodsModel::AirPods2), 2);
        QCOMPARE(int(AirPodsModel::AirPods3), 3);
        QCOMPARE(int(AirPodsModel::AirPodsPro), 4);
        QCOMPARE(int(AirPodsModel::AirPodsPro2Lightning), 5);
        QCOMPARE(int(AirPodsModel::AirPodsPro2USBC), 6);
        QCOMPARE(int(AirPodsModel::AirPodsMaxLightning), 7);
        QCOMPARE(int(AirPodsModel::AirPodsMaxUSBC), 8);
        QCOMPARE(int(AirPodsModel::AirPods4), 9);
        QCOMPARE(int(AirPodsModel::AirPods4ANC), 10);
        QCOMPARE(int(AirPodsModel::AirPodsPro3), 11);
        QCOMPARE(int(AirPodsModel::AirPodsMax2), 12);
        QCOMPARE(int(AirPodsModel::PowerbeatsPro), 13);
    }

    void isModelHeadset_onlyMax()
    {
        QVERIFY(isModelHeadset(AirPodsModel::AirPodsMaxLightning));
        QVERIFY(isModelHeadset(AirPodsModel::AirPodsMaxUSBC));
        QVERIFY(isModelHeadset(AirPodsModel::AirPodsMax2));
        QVERIFY(!isModelHeadset(AirPodsModel::AirPods1));
        QVERIFY(!isModelHeadset(AirPodsModel::AirPodsPro));
        QVERIFY(!isModelHeadset(AirPodsModel::AirPodsPro2USBC));
        QVERIFY(!isModelHeadset(AirPodsModel::AirPods4ANC));
        QVERIFY(!isModelHeadset(AirPodsModel::Unknown));
    }

    // Identity only since 2026-08-20, but a new Pro generation must still land here.
    void isProSeriesAirPods_coversAllProGens()
    {
        QVERIFY(isProSeriesAirPods(AirPodsModel::AirPodsPro));
        QVERIFY(isProSeriesAirPods(AirPodsModel::AirPodsPro2Lightning));
        QVERIFY(isProSeriesAirPods(AirPodsModel::AirPodsPro2USBC));
        QVERIFY(isProSeriesAirPods(AirPodsModel::AirPodsPro3));
        QVERIFY(!isProSeriesAirPods(AirPodsModel::AirPods1));
        QVERIFY(!isProSeriesAirPods(AirPodsModel::AirPods2));
        QVERIFY(!isProSeriesAirPods(AirPodsModel::AirPods3));
        QVERIFY(!isProSeriesAirPods(AirPodsModel::AirPods4));
        QVERIFY(!isProSeriesAirPods(AirPodsModel::AirPods4ANC));
        QVERIFY(!isProSeriesAirPods(AirPodsModel::AirPodsMaxLightning));
        QVERIFY(!isProSeriesAirPods(AirPodsModel::AirPodsMaxUSBC));
        QVERIFY(!isProSeriesAirPods(AirPodsModel::AirPodsMax2));
        QVERIFY(!isProSeriesAirPods(AirPodsModel::Unknown));
    }

    // The lineup as apple.com/airpods/compare listed it on 2026-08-20, one row per model.
    void capabilities_matchAppleCompareTable_data()
    {
        QTest::addColumn<int>("model");
        QTest::addColumn<bool>("noiseControl");
        QTest::addColumn<bool>("adaptive");
        QTest::addColumn<bool>("conversationalAwareness");
        QTest::addColumn<bool>("oneBudANC");

        QTest::newRow("AirPods1")        << int(AirPodsModel::AirPods1)             << false << false << false << false;
        QTest::newRow("AirPods2")        << int(AirPodsModel::AirPods2)             << false << false << false << false;
        QTest::newRow("AirPods3")        << int(AirPodsModel::AirPods3)             << false << false << false << false;
        QTest::newRow("AirPods4")        << int(AirPodsModel::AirPods4)             << false << false << false << false;
        QTest::newRow("AirPods4ANC")     << int(AirPodsModel::AirPods4ANC)          << true  << true  << true  << true;
        QTest::newRow("AirPodsPro")      << int(AirPodsModel::AirPodsPro)           << true  << false << false << true;
        QTest::newRow("AirPodsPro2L")    << int(AirPodsModel::AirPodsPro2Lightning) << true  << true  << true  << true;
        QTest::newRow("AirPodsPro2USBC") << int(AirPodsModel::AirPodsPro2USBC)      << true  << true  << true  << true;
        QTest::newRow("AirPodsPro3")     << int(AirPodsModel::AirPodsPro3)          << true  << true  << true  << true;
        QTest::newRow("AirPodsMaxL")     << int(AirPodsModel::AirPodsMaxLightning)  << true  << false << false << false;
        QTest::newRow("AirPodsMaxUSBC")  << int(AirPodsModel::AirPodsMaxUSBC)       << true  << false << false << false;
        QTest::newRow("AirPodsMax2")     << int(AirPodsModel::AirPodsMax2)          << true  << true  << true  << false;
        QTest::newRow("PowerbeatsPro")   << int(AirPodsModel::PowerbeatsPro)        << true  << false << false << true;
        // Unknown fails open on modes so an unmapped model keeps what it had, and gains nothing else.
        QTest::newRow("Unknown")         << int(AirPodsModel::Unknown)              << true  << false << false << false;
    }

    void capabilities_matchAppleCompareTable()
    {
        QFETCH(int, model);
        QFETCH(bool, noiseControl);
        QFETCH(bool, adaptive);
        QFETCH(bool, conversationalAwareness);
        QFETCH(bool, oneBudANC);

        const auto m = static_cast<AirPodsModel>(model);
        QCOMPARE(supportsNoiseControl(m), noiseControl);
        QCOMPARE(supportsAdaptiveAudio(m), adaptive);
        QCOMPARE(supportsConversationalAwareness(m), conversationalAwareness);
        QCOMPARE(supportsOneBudANC(m), oneBudANC);
    }

    // No listening feature can exist on a device with no listening modes.
    void capabilities_neverExceedNoiseControl()
    {
        for (int i = 0; i <= int(AirPodsModel::PowerbeatsPro); ++i) {
            const auto m = static_cast<AirPodsModel>(i);
            if (supportsNoiseControl(m)) continue;
            QVERIFY2(!supportsAdaptiveAudio(m) && !supportsConversationalAwareness(m) && !supportsOneBudANC(m),
                     qPrintable(QStringLiteral("model %1 has a listening feature without listening modes").arg(i)));
        }
    }

private:
    QString m_assetsDir;
};

QTEST_GUILESS_MAIN(TestEnums)
#include "tst_enums.moc"
