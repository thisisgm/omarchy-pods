#include <QtTest>
#include <QBluetoothAddress>
#include <QBluetoothDeviceInfo>

#include "../ble/blemanager.h"

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

class TestBleManager : public QObject
{
    Q_OBJECT

private slots:
    void unparseableFrameIsDropped_data();
    void unparseableFrameIsDropped();
    void realAirPodsFrameIsParsed();
    void powerbeatsFrameIsIdentified();
    void podsBatteryKeepsLeftAndRightApart();
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

void TestBleManager::powerbeatsFrameIsIdentified()
{
    QByteArray frame = QByteArray::fromHex(airPodsFrameHex);
    frame[3] = char(0x1D);
    frame[4] = char(0x20);
    BleInfo parsed;

    // The paired device reports product 0x201D in BlueZ's Modalias.
    QCOMPARE(parseFrame(frame, &parsed), 1);
    QCOMPARE(int(parsed.modelName), int(AirpodsTrayApp::Enums::AirPodsModel::PowerbeatsPro2));
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

QTEST_GUILESS_MAIN(TestBleManager)
#include "tst_blemanager.moc"
