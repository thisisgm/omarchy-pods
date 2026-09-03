#include <QtTest>
#include <QSettings>
#include <QTemporaryDir>

#include "../idlescan.hpp"

class TestIdleScan : public QObject
{
    Q_OBJECT

private slots:
    void defaultsToOn();
    void offWhenSetFalse();
};

static QString settingsPath(const QTemporaryDir &dir)
{
    return dir.filePath("AirPodsTrayApp.conf");
}

void TestIdleScan::defaultsToOn()
{
    QTemporaryDir dir;
    QSettings settings(settingsPath(dir), QSettings::IniFormat);

    QVERIFY(idleScanEnabled(settings));
}

void TestIdleScan::offWhenSetFalse()
{
    QTemporaryDir dir;
    {
        QSettings written(settingsPath(dir), QSettings::IniFormat);
        written.setValue("bluetooth/idleScan", false);
    }
    QSettings settings(settingsPath(dir), QSettings::IniFormat);

    QVERIFY(!idleScanEnabled(settings));
}

QTEST_GUILESS_MAIN(TestIdleScan)
#include "tst_idlescan.moc"
