// DeviceInfo persistence regression.
//
// Pre-fix, saveToSettings + loadFromSettings did not persist:
//   - conversationalAwareness
//   - oneBudANCMode
//   - adaptiveNoiseLevel
// User preferences silently reset to defaults on every daemon restart.
// New code persists them; this test does a save→reload round-trip in an
// isolated QSettings file and asserts the values survive.

#include <QTest>
#include <QSettings>
#include <QTemporaryFile>
#include <QLoggingCategory>

Q_LOGGING_CATEGORY(openpods, "openpods.test", QtWarningMsg)

#include "../deviceinfo.hpp"

class TestDeviceInfo : public QObject
{
    Q_OBJECT

private:
    QString m_iniPath;

private slots:
    void init()
    {
        QTemporaryFile tmp(QDir::tempPath() + "/openpods_ini_XXXXXX.ini");
        tmp.setAutoRemove(false);
        QVERIFY(tmp.open());
        m_iniPath = tmp.fileName();
        tmp.remove(); // we just want the unique path, write the file fresh
    }

    void cleanup()
    {
        QFile::remove(m_iniPath);
    }

    void persistsConversationalAwareness()
    {
        {
            DeviceInfo a;
            a.setConversationalAwareness(true);
            QSettings s(m_iniPath, QSettings::IniFormat);
            a.saveToSettings(s);
        }
        DeviceInfo b;
        QSettings s2(m_iniPath, QSettings::IniFormat);
        b.loadFromSettings(s2);
        QCOMPARE(b.conversationalAwareness(), true);
    }

    void persistsOneBudANCMode()
    {
        {
            DeviceInfo a;
            a.setOneBudANCMode(true);
            QSettings s(m_iniPath, QSettings::IniFormat);
            a.saveToSettings(s);
        }
        DeviceInfo b;
        QSettings s2(m_iniPath, QSettings::IniFormat);
        b.loadFromSettings(s2);
        QCOMPARE(b.oneBudANCMode(), true);
    }

    void persistsAdaptiveNoiseLevel()
    {
        {
            DeviceInfo a;
            a.setAdaptiveNoiseLevel(73);
            QSettings s(m_iniPath, QSettings::IniFormat);
            a.saveToSettings(s);
        }
        DeviceInfo b;
        QSettings s2(m_iniPath, QSettings::IniFormat);
        b.loadFromSettings(s2);
        QCOMPARE(b.adaptiveNoiseLevel(), 73);
    }

    void persistsDeviceNameAndModel()
    {
        {
            DeviceInfo a;
            a.setDeviceName("Gianmarco's AirPods Pro");
            a.setModel(AirPodsModel::AirPodsPro2USBC);
            QSettings s(m_iniPath, QSettings::IniFormat);
            a.saveToSettings(s);
        }
        DeviceInfo b;
        QSettings s2(m_iniPath, QSettings::IniFormat);
        b.loadFromSettings(s2);
        QCOMPARE(b.deviceName(), QStringLiteral("Gianmarco's AirPods Pro"));
        QCOMPARE(b.model(), AirPodsModel::AirPodsPro2USBC);
    }

    void defaultsWhenSettingsAbsent()
    {
        DeviceInfo b;
        QSettings empty(m_iniPath, QSettings::IniFormat); // file doesn't exist
        b.loadFromSettings(empty);
        // Defaults defined in the class body.
        QCOMPARE(b.conversationalAwareness(), false);
        QCOMPARE(b.oneBudANCMode(), false);
        QCOMPARE(b.adaptiveNoiseLevel(), 50);
        QCOMPARE(b.deviceName(), QString());
        QCOMPARE(b.model(), AirPodsModel::Unknown);
    }

    void resetDoesNotClearUserPrefs()
    {
        DeviceInfo a;
        a.setConversationalAwareness(true);
        a.setOneBudANCMode(true);
        a.setAdaptiveNoiseLevel(80);
        a.setDeviceName("Pods");

        a.reset();

        // reset() clears connection-state but keeps user preferences
        // because they're loaded from QSettings on next start anyway.
        // The current code only resets connection-state fields.
        QCOMPARE(a.conversationalAwareness(), true);
        QCOMPARE(a.oneBudANCMode(), true);
        QCOMPARE(a.adaptiveNoiseLevel(), 80);
        QCOMPARE(a.deviceName(), QString()); // device name IS reset
    }

    void resetClearsModelNumber()
    {
        DeviceInfo a;
        a.setModelNumber("A3063");
        a.setModel(AirPodsModel::AirPodsPro3);

        a.reset();

        // A pair that sends no AAP metadata would otherwise report the previous pair's model number.
        QCOMPARE(a.modelNumber(), QString());
        QCOMPARE(a.model(), AirPodsModel::Unknown);
    }
};

QTEST_GUILESS_MAIN(TestDeviceInfo)
#include "tst_deviceinfo.moc"
