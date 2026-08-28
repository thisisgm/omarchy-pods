#ifndef BLEMANAGER_H
#define BLEMANAGER_H

#include <QObject>
#include <QBluetoothDeviceDiscoveryAgent>
#include <QMap>
#include <QString>
#include <QDateTime>
#include "enums.h"

class QTimer;

class BleInfo
{
public:
    QString name;
    QString address;
    int leftPodBattery = -1; // -1 indicates not available
    int rightPodBattery = -1;
    int caseBattery = -1;
    bool leftCharging = false;
    bool rightCharging = false;
    bool caseCharging = false;
    AirpodsTrayApp::Enums::AirPodsModel modelName = AirpodsTrayApp::Enums::AirPodsModel::Unknown;
    quint8 lidOpenCounter = 0;
    QString color = "Unknown"; // Default color
    quint8 status = 0;
    QByteArray rawData;
    QByteArray encryptedPayload; // 16 bytes of encrypted payload

    // Additional status flags from Kotlin version
    bool isLeftPodInEar = false;
    bool isRightPodInEar = false;
    bool isPrimaryInEar = false;
    bool isSecondaryInEar = false;
    bool isLeftPodMicrophone = false;
    bool isRightPodMicrophone = false;
    bool isThisPodInTheCase = false;
    bool isOnePodInCase = false;
    bool areBothPodsInCase = false;
    bool primaryLeft = true; // True if left pod is primary, false if right pod is primary

    // Lid state enumeration
    enum class LidState
    {
        OPEN = 0x0,
        CLOSED = 0x1,
        UNKNOWN,
    } lidState = LidState::UNKNOWN;

    // Connection state enumeration
    enum class ConnectionState : uint8_t
    {
        DISCONNECTED = 0x00,
        IDLE = 0x04,
        MUSIC = 0x05,
        CALL = 0x06,
        RINGING = 0x07,
        HANGING_UP = 0x09,
        UNKNOWN = 0xFF // Using 0xFF for representing null in the original
    } connectionState = ConnectionState::UNKNOWN;

    QDateTime lastSeen; // Timestamp of last detection
};

class BleManager : public QObject
{
    Q_OBJECT
public:
    explicit BleManager(QObject *parent = nullptr);
    ~BleManager();

    void startScan();
    void stopScan();
    bool isScanning() const;

    // Keep the radio dark for `ms` even if a scan is requested. Bonded
    // devices reconnect in the first seconds after resume and after the
    // adapter powers up; a burst landing in that window is what leaves a
    // keyboard dead until Bluetooth is toggled.
    void holdOff(int ms);

private slots:
    void onDeviceDiscovered(const QBluetoothDeviceInfo &info);
    void onScanFinished();
    void onErrorOccurred(QBluetoothDeviceDiscoveryAgent::Error error);

signals:
    void deviceFound(const BleInfo &device);

private:
    // fromCycleTick: the repeating cycle is one-per-period by construction and
    // needs no elapsed-time gate; caller-initiated starts do, and are held to a
    // full period since the last burst.
    void beginBurst(bool fromCycleTick);

    // Default-init so a partial construction (or a refactor that
    // skips the explicit ctor body) doesn't leave a dangling pointer
    // that start/stop/isScan would dereference. Real assignment
    // happens in BleManager::BleManager() via parented `new`.
    QBluetoothDeviceDiscoveryAgent *discoveryAgent = nullptr;

    // A BlueZ discovery session keeps the controller in inquiry, and a
    // controller stuck in inquiry cannot service the connection
    // requests of other bonded devices — a keyboard trying to reconnect
    // at boot waits forever behind a scan that never yields. So the
    // scan runs in bursts on a fixed cycle clock that callers cannot
    // reset: the control-link recovery ladder legitimately stops and
    // restarts the scan every few seconds while pods are near but not
    // connected, and letting each restart begin a fresh burst pinned
    // the radio in inquiry exactly as if there were no duty cycle.
    QTimer *cycleTimer = nullptr; // repeating, one burst per period
    QTimer *burstTimer = nullptr; // single-shot, ends the burst
    bool scanRequested = false;   // callers' intent, held across bursts
    qint64 lastBurstStartMs = -1; // CLOCK_MONOTONIC, gates early bursts
    qint64 holdOffUntilMs = -1;   // CLOCK_MONOTONIC, no bursts before this
};

#endif // BLEMANAGER_H