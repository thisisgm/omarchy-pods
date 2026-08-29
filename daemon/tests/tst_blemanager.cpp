#include <QtTest>
#include <QBluetoothAddress>
#include <QBluetoothDeviceDiscoveryAgent>
#include <QBluetoothDeviceInfo>
#include <QTimer>

#include "../ble/blemanager.h"
#include "../ble/blescanretry.hpp"

// main.cpp defines the Q_LOGGING_CATEGORY symbol, but tests don't link main.cpp.
Q_LOGGING_CATEGORY(openpods, "openpods.test", QtWarningMsg)

// Captured live: 07 19 01 27 20 21 88 8f 11 00 04 then the 16-byte encrypted payload.
static const char *airPodsFrameHex = "071901272021888f110004b48a83d66c322a4745cb15da3fd6ab2b";
// Captured in the same scan from another Apple device, 19 bytes and so under the fixed layout.
static const char *otherAppleFrameHex = "071106e4dd8d647035112e2b36d341a6778984";

static QBluetoothDeviceInfo advertisement(const QByteArray &data)
{
    QBluetoothDeviceInfo info(QBluetoothAddress("00:11:22:33:44:55"), QStringLiteral("AirPods"), 0);
    info.setManufacturerData(0x004C, data);
    return info;
}

// 0x10 is Apple's nearby info type, long enough to pass the size check and still not ours.
static QByteArray typeSwapped(const QByteArray &frame)
{
    QByteArray other = frame;
    other[0] = char(0x10);
    return other;
}

// data[6] carries both pods, one nibble each, so an unequal byte separates them.
static QByteArray withPodsBattery(const QByteArray &frame, quint8 podsByte)
{
    QByteArray other = frame;
    other[6] = char(podsByte);
    return other;
}

// Bit 5 of the status byte says the left pod is primary, and clearing it flips the nibbles.
static QByteArray withRightPodPrimary(const QByteArray &frame)
{
    QByteArray other = frame;
    other[5] = char(quint8(other[5]) & ~0x20);
    return other;
}

// Emissions for one frame, or -1 when the slot could not be invoked at all.
static int parseFrame(const QByteArray &frame, BleInfo *emitted)
{
    BleManager manager;
    int emissions = 0;
    QObject::connect(&manager, &BleManager::deviceFound, &manager, [&](const BleInfo &device) {
        if (emitted)
            *emitted = device;
        ++emissions;
    });

    if (!QMetaObject::invokeMethod(&manager, "onDeviceDiscovered", Qt::DirectConnection,
                                   Q_ARG(QBluetoothDeviceInfo, advertisement(frame))))
        return -1;

    return emissions;
}

// PoweredOffError is the error a cold start sees when the daemon starts before the adapter.
static void failScan(BleManager &manager)
{
    QMetaObject::invokeMethod(&manager, "onErrorOccurred", Qt::DirectConnection,
                              Q_ARG(QBluetoothDeviceDiscoveryAgent::Error,
                                    QBluetoothDeviceDiscoveryAgent::PoweredOffError));
}

// start() can fail at once on a box with no adapter, so reset the count with an advertisement first.
static void seeAdvertisement(BleManager &manager)
{
    QMetaObject::invokeMethod(&manager, "onDeviceDiscovered", Qt::DirectConnection,
                              Q_ARG(QBluetoothDeviceInfo,
                                    advertisement(QByteArray::fromHex(airPodsFrameHex))));
}

// discoveryAgent is the only other direct child, and it is not a QTimer.
static QTimer *retryTimerOf(BleManager &manager)
{
    return manager.findChild<QTimer *>(QString(), Qt::FindDirectChildrenOnly);
}

class TestBleManager : public QObject
{
    Q_OBJECT

private slots:
    void unparseableFrameIsDropped_data();
    void unparseableFrameIsDropped();
    void realAirPodsFrameIsParsed();
    void podsBatteryKeepsLeftAndRightApart();
    void retryDelayDoublesAndCaps();
    void scanErrorArmsARetryAndKeepsTheIntent();
    void advertisementResetsTheRetryLadder();
    void stoppedScanIsNotRestartedByAFailure();
};

void TestBleManager::unparseableFrameIsDropped_data()
{
    const QByteArray airPods = QByteArray::fromHex(airPodsFrameHex);

    QTest::addColumn<QByteArray>("frame");
    QTest::newRow("empty payload") << QByteArray();
    QTest::newRow("10 bytes, one past the end at the data[10] read") << airPods.left(10);
    QTest::newRow("19 bytes, a real Apple advertisement under the fixed layout") << QByteArray::fromHex(otherAppleFrameHex);
    QTest::newRow("26 bytes, one short of the fixed layout") << airPods.left(26);
    QTest::newRow("full length but not a proximity pairing type") << typeSwapped(airPods);
}

void TestBleManager::unparseableFrameIsDropped()
{
    QFETCH(QByteArray, frame);

    QCOMPARE(parseFrame(frame, nullptr), 0);
}

void TestBleManager::realAirPodsFrameIsParsed()
{
    BleInfo parsed;

    QCOMPARE(parseFrame(QByteArray::fromHex(airPodsFrameHex), &parsed), 1);
    // Captured from the operator's own AirPods Pro 3: data[3..4] is 27 20, so the key is 0x2720.
    QCOMPARE(int(parsed.modelName), int(AirpodsTrayApp::Enums::AirPodsModel::AirPodsPro3));
    // Decoded from the capture: 0x8f is a case nibble of 15 meaning absent, 0x04 is idle.
    QCOMPARE(parsed.caseBattery, -1);
    QCOMPARE(parsed.connectionState, BleInfo::ConnectionState::IDLE);
}

void TestBleManager::podsBatteryKeepsLeftAndRightApart()
{
    const QByteArray unequalPods = withPodsBattery(QByteArray::fromHex(airPodsFrameHex), 0x82);
    BleInfo parsed;

    // The capture's status byte 0x21 has bit 5 set, so the low nibble belongs to the left pod.
    QCOMPARE(parseFrame(unequalPods, &parsed), 1);
    QCOMPARE(parsed.leftPodBattery, 20);
    QCOMPARE(parsed.rightPodBattery, 80);

    QCOMPARE(parseFrame(withRightPodPrimary(unequalPods), &parsed), 1);
    QCOMPARE(parsed.leftPodBattery, 80);
    QCOMPARE(parsed.rightPodBattery, 20);
}

void TestBleManager::retryDelayDoublesAndCaps()
{
    QCOMPARE(BleScanRetry::delayMs(1), 1000);
    QCOMPARE(BleScanRetry::delayMs(2), 2000);
    QCOMPARE(BleScanRetry::delayMs(BleScanRetry::maxDoublings), 32000);
    QCOMPARE(BleScanRetry::delayMs(BleScanRetry::maxDoublings + 5), 32000);
    // An attempt of zero still gets the first delay.
    QCOMPARE(BleScanRetry::delayMs(0), 1000);

    BleScanRetry::Ladder ladder;
    QCOMPARE(ladder.nextDelayMs(), 1000);
    QCOMPARE(ladder.nextDelayMs(), 2000);
    QCOMPARE(ladder.attempts(), 2);
    ladder.reset();
    QCOMPARE(ladder.nextDelayMs(), 1000);
}

// The cold start in issue 24. The adapter is down, the scan fails, and nothing starts it again.
void TestBleManager::scanErrorArmsARetryAndKeepsTheIntent()
{
    BleManager manager;

    manager.startScan();
    seeAdvertisement(manager);
    failScan(manager);

    // The defect. One error stopped the scan for the life of the process.
    QVERIFY(manager.isScanning());

    QTimer *retry = retryTimerOf(manager);
    QVERIFY(retry);
    QVERIFY(retry->isActive());
    QCOMPARE(retry->interval(), BleScanRetry::delayMs(1));

    manager.stopScan();
}

// Each connect and disconnect starts the scan again, so a new failure must not keep the old delay.
void TestBleManager::advertisementResetsTheRetryLadder()
{
    BleManager manager;
    QTimer *retry = retryTimerOf(manager);
    QVERIFY(retry);

    manager.startScan();
    seeAdvertisement(manager);
    failScan(manager);
    failScan(manager);
    QCOMPARE(retry->interval(), BleScanRetry::delayMs(2));

    seeAdvertisement(manager);
    failScan(manager);
    QCOMPARE(retry->interval(), BleScanRetry::delayMs(1));

    manager.stopScan();
}

// The connected gate and the sleep gate stop the scan on purpose. A retry must not undo that.
void TestBleManager::stoppedScanIsNotRestartedByAFailure()
{
    BleManager manager;
    QTimer *retry = retryTimerOf(manager);
    QVERIFY(retry);

    manager.startScan();
    QVERIFY(manager.isScanning());
    seeAdvertisement(manager);
    failScan(manager);
    QVERIFY(retry->isActive());

    manager.stopScan();
    QVERIFY(!retry->isActive());
    QVERIFY(!manager.isScanning());

    failScan(manager);
    QVERIFY(!retry->isActive());
    QVERIFY(!manager.isScanning());
}

QTEST_GUILESS_MAIN(TestBleManager)
#include "tst_blemanager.moc"
