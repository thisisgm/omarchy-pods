#include <QSettings>
#include <QLocalServer>
#include <QSaveFile>
#include <QFileInfo>
#include <QLocalSocket>
#include <QApplication>
#include <QQmlApplicationEngine>
#include <QQuickStyle>
#include <QStyleHints>
#include <QPalette>
#include <QJsonObject>
#include <QJsonDocument>
#include <QSocketNotifier>
#include <QQmlContext>
#include <csignal>
#include <fcntl.h>
#include <memory>
#include <unistd.h>
#include <QBluetoothLocalDevice>
#include <QBluetoothSocket>
#include <QQuickWindow>
#include <QLoggingCategory>
#include <QThread>
#include <QTimer>
#include <QProcess>
#include <QRegularExpression>
#include <QTranslator>
#include <QLibraryInfo>
#include <QDir>
#include <QStandardPaths>

#include "airpods_packets.h"
#include "logger.h"
#include "media/mediacontroller.h"
#include "trayiconmanager.h"
#include "notifier.hpp"
#include "enums.h"
#include <QRandomGenerator>
#include "battery.hpp"
#include "lowbatterywatcher.hpp"
#include "BluetoothMonitor.h"
#include "autostartmanager.hpp"
#include "deviceinfo.hpp"
#include "ipcpath.hpp"
#include "ipcverb.hpp"
#include "ble/blemanager.h"
#include "ble/bleutils.h"
#include "QRCodeImageProvider.hpp"
#include "systemsleepmonitor.hpp"
#include "applecontroller.hpp"
#include "controlreconnect.hpp"

using namespace AirpodsTrayApp::Enums;

// Symbol and journald category string both `openpods` (renamed iter-51).
// Users filter via QT_LOGGING_RULES with `openpods.*`.
Q_LOGGING_CATEGORY(openpods, "openpods")

class AirPodsTrayApp : public QObject {
    Q_OBJECT
    Q_PROPERTY(bool airpodsConnected READ areAirpodsConnected NOTIFY airPodsStatusChanged)
    Q_PROPERTY(int earDetectionBehavior READ earDetectionBehavior WRITE setEarDetectionBehavior NOTIFY earDetectionBehaviorChanged)
    Q_PROPERTY(bool crossDeviceEnabled READ crossDeviceEnabled WRITE setCrossDeviceEnabled NOTIFY crossDeviceEnabledChanged)
    Q_PROPERTY(AutoStartManager *autoStartManager READ autoStartManager CONSTANT)
    Q_PROPERTY(bool notificationsEnabled READ notificationsEnabled WRITE setNotificationsEnabled NOTIFY notificationsEnabledChanged)
    Q_PROPERTY(int retryAttempts READ retryAttempts WRITE setRetryAttempts NOTIFY retryAttemptsChanged)
    Q_PROPERTY(bool hideOnStart READ hideOnStart CONSTANT)
    Q_PROPERTY(DeviceInfo *deviceInfo READ deviceInfo CONSTANT)
    Q_PROPERTY(QString phoneMacStatus READ phoneMacStatus NOTIFY phoneMacStatusChanged)
    Q_PROPERTY(bool hearingAidEnabled READ hearingAidEnabled WRITE setHearingAidEnabled NOTIFY hearingAidEnabledChanged)

public:
    AirPodsTrayApp(bool debugMode, bool hideOnStart, bool headless, QQmlApplicationEngine *parent = nullptr)
        : QObject(parent), debugMode(debugMode), m_settings(new QSettings("AirPodsTrayApp", "AirPodsTrayApp"))
        , m_autoStartManager(new AutoStartManager(this)), m_hideOnStart(hideOnStart), parent(parent)
        , m_deviceInfo(new DeviceInfo(this)), m_bleManager(new BleManager(this))
        , m_systemSleepMonitor(new SystemSleepMonitor(this)), m_notifier(new Notifier(this))
    {
        QLoggingCategory::setFilterRules(QString("openpods.debug=%1").arg(debugMode ? "true" : "false"));
        LOG_INFO("Initializing OpenPods");
        restrictSettingsAccess();

        m_notifier->setEnabled(loadNotificationsEnabled());
        connect(m_notifier, &Notifier::enabledChanged, this, &AirPodsTrayApp::saveNotificationsEnabled);
        connect(m_notifier, &Notifier::enabledChanged, this, &AirPodsTrayApp::notificationsEnabledChanged);
        connect(m_deviceInfo, &DeviceInfo::batteryStatusChanged, this, &AirPodsTrayApp::checkLowBatteryThresholds);

        // Headless has no tray to take the fallback, so say so rather than dropping the toast silently.
        if (headless) {
            connect(m_notifier, &Notifier::fallbackRequested, this, [](const QString &title, const QString &message) {
                LOG_WARN("omarchy is not on PATH, so this notification went nowhere: " << title << " " << message);
            });
        }

        // A headless run has no QSystemTrayIcon, which is what keeps Qt Widgets and Gui off this process.
        if (!headless) {
            trayManager = new TrayIconManager(this);
            connect(m_notifier, &Notifier::fallbackRequested, trayManager, &TrayIconManager::showTrayMessage);
            connect(trayManager, &TrayIconManager::trayClicked, this, &AirPodsTrayApp::onTrayIconActivated);
            connect(trayManager, &TrayIconManager::middleClicked, this, &AirPodsTrayApp::cycleNoiseControlMode);
            connect(trayManager, &TrayIconManager::openApp, this, &AirPodsTrayApp::onOpenApp);
            connect(trayManager, &TrayIconManager::openSettings, this, &AirPodsTrayApp::onOpenSettings);
            connect(trayManager, &TrayIconManager::noiseControlChanged, this, &AirPodsTrayApp::setNoiseControlMode);
            connect(trayManager, &TrayIconManager::conversationalAwarenessToggled, this, &AirPodsTrayApp::setConversationalAwareness);
            connect(m_deviceInfo, &DeviceInfo::batteryStatusChanged, trayManager, &TrayIconManager::updateBatteryStatus);
            connect(m_deviceInfo, &DeviceInfo::noiseControlModeChanged, trayManager, &TrayIconManager::updateNoiseControlState);
            connect(m_deviceInfo, &DeviceInfo::conversationalAwarenessChanged, trayManager, &TrayIconManager::updateConversationalAwareness);
        }

        // Initialize MediaController and connect signals
        mediaController = new MediaController(this);
        connect(mediaController, &MediaController::mediaStateChanged, this, &AirPodsTrayApp::handleMediaStateChange);
        mediaController->followMediaChanges();

        monitor = new BluetoothMonitor(this);
        connect(monitor, &BluetoothMonitor::deviceConnected, this, &AirPodsTrayApp::bluezDeviceConnected);
        connect(monitor, &BluetoothMonitor::deviceDisconnected, this, &AirPodsTrayApp::bluezDeviceDisconnected);
        connect(monitor, &BluetoothMonitor::deviceConnectionProbeFinished,
                this, &AirPodsTrayApp::controlConnectionProbeFinished);

        // The control socket can drop while BlueZ keeps A2DP up, so Device1::Connected never rises again.
        m_controlReconnectTimer = new QTimer(this);
        m_controlReconnectTimer->setSingleShot(true);
        connect(m_controlReconnectTimer, &QTimer::timeout,
                this, &AirPodsTrayApp::attemptControlReconnect);

        // Its bounded retries (m_retryAttempts) can still exhaust with BlueZ sitting on
        // Connected the whole time (no transition ever arrives to retrigger recovery) -
        // this periodic check is the fallback for that case. See checkControlLinkWatchdog().
        m_controlWatchdogTimer = new QTimer(this);
        m_controlWatchdogTimer->setInterval(ControlReconnect::watchdogIntervalMs);
        connect(m_controlWatchdogTimer, &QTimer::timeout,
                this, &AirPodsTrayApp::checkControlLinkWatchdog);
        m_controlWatchdogTimer->start();

        connect(m_bleManager, &BleManager::deviceFound, this, &AirPodsTrayApp::bleDeviceFound);
        connect(m_deviceInfo->getBattery(), &Battery::primaryChanged, this, &AirPodsTrayApp::primaryChanged);
        connect(m_systemSleepMonitor, &SystemSleepMonitor::systemGoingToSleep, this, &AirPodsTrayApp::onSystemGoingToSleep);
        connect(m_systemSleepMonitor, &SystemSleepMonitor::systemWakingUp, this, &AirPodsTrayApp::onSystemWakingUp);

        // Load settings
        CrossDevice.isEnabled = loadCrossDeviceEnabled();
        setEarDetectionBehavior(loadEarDetectionSettings());
        setRetryAttempts(loadRetryAttempts());
        // Restore persisted PhoneMAC into the process env BEFORE
        // connectToPhone() reads it. setPhoneMac persists to QSettings
        // (iter-bugfix) so user doesn't have to re-enter on every
        // restart. If the saved value is invalid (manually edited
        // settings file), setPhoneMac surfaces the validation error.
        if (m_settings) {
            const QString savedMac = m_settings->value(QStringLiteral("phoneMac"), QString()).toString();
            if (!savedMac.isEmpty()) {
                setPhoneMac(savedMac);
            }
        }

        monitor->checkAlreadyConnectedDevices();
        LOG_INFO("AirPodsTrayApp initialized");

        QBluetoothLocalDevice localDevice;

        const QList<QBluetoothAddress> connectedDevices = localDevice.connectedDevices();
        for (const QBluetoothAddress &address : connectedDevices) {
            QBluetoothDeviceInfo device(address, "", 0);
            if (isAirPodsDevice(device)) {
                connectToDevice(device);

                // On startup after reboot, activate A2DP profile for already connected AirPods
                QTimer::singleShot(2000, this, [this, address]()
                {
                    // A disconnect inside these two seconds would otherwise start a chain for a device that left.
                    if (!areAirpodsConnected())
                    {
                        LOG_INFO("AirPods disconnected before the startup A2DP activation, skipping it");
                        return;
                    }

                    QString formattedAddress = address.toString().replace(":", "_");
                    LOG_INFO("A2DP profile activation attempted for AirPods found on startup");
                    mediaController->activateA2dpProfileWithRetry(formattedAddress);
                });
                return;
            }
        }

        initializeDBus();
        initializeBluetooth();
    }

    ~AirPodsTrayApp() override {
        saveCrossDeviceEnabled();
        saveEarDetectionSettings();

        // Reliability summary on shutdown so journald has a trendline
        // even without an exposed metrics endpoint.
        LOG_INFO("Reliability counters: reconnect_attempts_total="
                 << m_reconnectAttemptsTotal
                 << " reconnect_failures_total=" << m_reconnectFailuresTotal
                 << " noise_control_changes_total=" << m_noiseControlChangesTotal
                 << " forget_calls_total=" << m_forgetCallsTotal
                 << " ear_detection_changes_total=" << m_earDetectionChangesTotal
                 << " ca_changes_total=" << m_caChangesTotal
                 << " disconnect_calls_total=" << m_disconnectCallsTotal
                 << " connect_calls_total=" << m_connectCallsTotal
                 << " disconnect_failures_total=" << m_disconnectFailuresTotal
                 << " connect_failures_total=" << m_connectFailuresTotal
                 << " adaptive_level_changes_total=" << m_adaptiveLevelChangesTotal
                 << " one_bud_anc_changes_total=" << m_oneBudANCChangesTotal
                 << " reopen_calls_total=" << m_reopenCallsTotal);

        // Use deleteLater so any queued signal already in flight finishes
        // dispatching before the QObject vanishes. A bare `delete` here
        // races with the QBluetoothSocket's own emit chain.
        if (socket) {
            socket->disconnect(this);
            socket->deleteLater();
            socket = nullptr;
        }
        if (phoneSocket) {
            phoneSocket->disconnect(this);
            phoneSocket->deleteLater();
            phoneSocket = nullptr;
        }
    }

    bool areAirpodsConnected() const { return socket && socket->isOpen() && socket->state() == QBluetoothSocket::SocketState::ConnectedState; }
    int earDetectionBehavior() const { return mediaController->getEarDetectionBehavior(); }
    bool crossDeviceEnabled() const { return CrossDevice.isEnabled; }
    AutoStartManager *autoStartManager() const { return m_autoStartManager; }
    bool notificationsEnabled() const { return m_notifier->enabled(); }
    void setNotificationsEnabled(bool enabled) { m_notifier->setEnabled(enabled); }
    int retryAttempts() const { return m_retryAttempts; }
    bool hideOnStart() const { return m_hideOnStart; }
    DeviceInfo *deviceInfo() const { return m_deviceInfo; }
    QString phoneMacStatus() const { return m_phoneMacStatus; }
    bool hearingAidEnabled() const { return m_deviceInfo->hearingAidEnabled(); }

private:
    bool debugMode;
    bool isConnectedLocally = false;

    QQmlApplicationEngine *parent = nullptr;

    struct {
        bool isAvailable = true;
        bool isEnabled = true; // Ability to disable the feature
    } CrossDevice;

    void initializeDBus() { }

    bool isAirPodsDevice(const QBluetoothDeviceInfo &device)
    {
        return device.serviceUuids().contains(QBluetoothUuid("74ec2172-0bad-4d01-8f77-997b2be0722a"));
    }

    void notifyAndroidDevice()
    {
        if (!CrossDevice.isEnabled) {
            return;
        }

        if (phoneSocket && phoneSocket->isOpen())
        {
            phoneSocket->write(AirPodsPackets::Phone::NOTIFICATION);
            LOG_DEBUG("Sent notification packet to Android: " << AirPodsPackets::Phone::NOTIFICATION.toHex());
        }
        else
        {
            LOG_WARN("Phone socket is not open, cannot send notification packet");
        }
    }

    void disconnectDevice(const QString &devicePath) {
        LOG_INFO("Disconnecting device at " << devicePath);
    }

public slots:
    // Apple-style Disconnect / Connect controls. Wrap `bluetoothctl`
    // with the saved BD_ADDR. Async QProcess + deleteLater so the Qt
    // event loop doesn't block on the BlueZ IO round-trip. Refuses if
    // the daemon hasn't learned an address yet (initial pairing not
    // done). The connect path also triggers the existing
    // bluezDeviceConnected handler, which re-runs the AAP handshake.
    void disconnectAirPods() {
        if (!m_deviceInfo || m_deviceInfo->bluetoothAddress().isEmpty()) {
            LOG_WARN("disconnectAirPods: no current address to disconnect");
            return;
        }
        const QString addr = m_deviceInfo->bluetoothAddress();
        LOG_INFO("disconnectAirPods: " << addr);
        ++m_disconnectCallsTotal;
        auto *proc = new QProcess(this);
        connect(proc, QOverload<int, QProcess::ExitStatus>::of(&QProcess::finished),
                proc, [this, proc, addr](int code, QProcess::ExitStatus) {
                    // INFO only on failure so journald isn't spammed
                    // with one success line per disconnect; the per-IPC
                    // disconnectAirPods entry above already records
                    // intent. Failures still surface for triage.
                    if (code != 0) {
                        LOG_INFO("bluetoothctl disconnect " << addr << " exit=" << code
                                 << " out=" << proc->readAllStandardOutput().trimmed());
                        ++m_disconnectFailuresTotal;
                    } else {
                        LOG_DEBUG("bluetoothctl disconnect " << addr << " ok");
                    }
                    proc->deleteLater();
                });
        proc->start("bluetoothctl", QStringList() << "disconnect" << addr);
    }

    void connectAirPods() {
        if (!m_deviceInfo || m_deviceInfo->bluetoothAddress().isEmpty()) {
            LOG_WARN("connectAirPods: no current address to connect");
            return;
        }
        const QString addr = m_deviceInfo->bluetoothAddress();
        LOG_INFO("connectAirPods: " << addr);
        ++m_connectCallsTotal;
        auto *proc = new QProcess(this);
        connect(proc, QOverload<int, QProcess::ExitStatus>::of(&QProcess::finished),
                proc, [this, proc, addr](int code, QProcess::ExitStatus) {
                    // Same pattern as disconnect lambda: INFO on
                    // failure only, DEBUG on the success path. Most
                    // connect calls succeed; spam reduction matters.
                    if (code != 0) {
                        LOG_INFO("bluetoothctl connect " << addr << " exit=" << code
                                 << " out=" << proc->readAllStandardOutput().trimmed());
                        ++m_connectFailuresTotal;
                    } else {
                        LOG_DEBUG("bluetoothctl connect " << addr << " ok");
                    }
                    proc->deleteLater();
                });
        proc->start("bluetoothctl", QStringList() << "connect" << addr);
    }

    // Apple "Forget This Device" parity. Walks bluetoothctl with the
    // currently-connected BD_ADDR so the kernel BlueZ stack drops the
    // pairing keys + cached profile data. Caller is responsible for
    // confirming intent — there's no in-process undo.
    //
    // Async (own QProcess + deleteLater on finish) so we don't block
    // the Qt event loop on bluetoothctl's IO. The address comes from
    // DeviceInfo; refuses if no device is currently associated.
    void forgetDevice() {
        if (!m_deviceInfo || m_deviceInfo->bluetoothAddress().isEmpty()) {
            LOG_WARN("forgetDevice: no current device address to forget");
            return;
        }
        const QString addr = m_deviceInfo->bluetoothAddress();
        LOG_INFO("Forgetting device: " << addr);
        ++m_forgetCallsTotal;
        auto *proc = new QProcess(this);
        connect(proc, QOverload<int, QProcess::ExitStatus>::of(&QProcess::finished),
                proc, [proc, addr](int code, QProcess::ExitStatus) {
                    if (code == 0) {
                        LOG_INFO("bluetoothctl remove " << addr << " ok: "
                                 << proc->readAllStandardOutput().trimmed());
                    } else {
                        LOG_ERROR("bluetoothctl remove " << addr << " exit " << code
                                  << ": " << proc->readAllStandardError().trimmed());
                    }
                    proc->deleteLater();
                });
        proc->start("bluetoothctl", QStringList() << "remove" << addr);
    }

    void connectToDevice(const QString &address) {
        LOG_INFO("Connecting to device with address: " << address);
        QBluetoothAddress btAddress(address);
        QBluetoothDeviceInfo device(btAddress, "", 0);
        connectToDevice(device);
    }

    void setNoiseControlMode(NoiseControlMode mode)
    {
        if (m_deviceInfo->noiseControlMode() == mode)
        {
            LOG_DEBUG("Noise control mode is already set to: " << static_cast<int>(mode));
            return;
        }
        LOG_INFO("Setting noise control mode to: " << mode);
        QByteArray packet = AirPodsPackets::NoiseControl::getPacketForMode(mode);
        writePacketToSocket(packet, "Noise control mode packet written: ");
        ++m_noiseControlChangesTotal;
    }
    void setNoiseControlModeInt(int mode)
    {
        if (mode < 0 || mode > static_cast<int>(NoiseControlMode::Adaptive))
        {
            LOG_ERROR("Invalid noise control mode: " << mode);
            return;
        }
        setNoiseControlMode(static_cast<NoiseControlMode>(mode));
    }

    // Walk every battery source on each battery-status change and
    // delegate the trip/reset decision to its LowBatteryLatch. Format
    // + emit the user-visible toast here; the latch owns no UI. The
    // global notificationsEnabled setting still gates emission via
    // Notifier::notify.
    void checkLowBatteryThresholds()
    {
        if (!m_deviceInfo) return;
        Battery *b = m_deviceInfo->getBattery();
        if (!b) return;

        struct Source {
            const char *label;
            quint8 level;
            bool available;
            bool charging;
            LowBatteryLatch *latch;
        };
        const Source sources[] = {
            { "Left",  b->getLeftPodLevel(),  b->isLeftPodAvailable(),
              b->isLeftPodCharging(),  &m_lowBatteryLatchLeft },
            { "Right", b->getRightPodLevel(), b->isRightPodAvailable(),
              b->isRightPodCharging(), &m_lowBatteryLatchRight },
            { "Case",  b->getCaseLevel(),     b->isCaseAvailable(),
              b->isCaseCharging(),     &m_lowBatteryLatchCase },
        };

        for (const Source &s : sources)
        {
            const auto fire = s.latch->evaluate(s.level, s.available, s.charging);
            if (fire) {
                m_notifier->notify(
                    tr("%1 AirPod Low Battery").arg(QString::fromLatin1(s.label)),
                    tr("%1% remaining").arg(*fire));
            }
        }
    }

    // Cycle the noise-control mode in the same order the PodsMenu
    // segmented displays: Off -> NoiseCancellation -> Transparency ->
    // Adaptive -> Off. Bound to the tray icon middle-click and to the
    // "noise:cycle" IPC verb so other surfaces (keyboard shortcuts,
    // openpods-ctl) can reuse the rotation without duplicating the
    // state machine.
    void cycleNoiseControlMode()
    {
        if (!m_deviceInfo)
        {
            LOG_ERROR("Cannot cycle noise control mode: device info not ready");
            return;
        }
        const int current = static_cast<int>(m_deviceInfo->noiseControlMode());
        const int next = (current + 1) % (static_cast<int>(NoiseControlMode::Adaptive) + 1);
        setNoiseControlModeInt(next);
    }

    void setConversationalAwareness(bool enabled)
    {
        if (!m_deviceInfo) {
            LOG_WARN("setConversationalAwareness: m_deviceInfo not ready");
            return;
        }
        if (m_deviceInfo->conversationalAwareness() == enabled) {
            LOG_DEBUG("Conversational awareness already " << (enabled ? "enabled" : "disabled"));
            return;
        }
        LOG_INFO("Setting conversational awareness to: " << (enabled ? "enabled" : "disabled"));
        QByteArray packet = enabled ? AirPodsPackets::ConversationalAwareness::ENABLED
                                    : AirPodsPackets::ConversationalAwareness::DISABLED;

        writePacketToSocket(packet, "Conversational awareness packet written: ");
        m_deviceInfo->setConversationalAwareness(enabled);
        ++m_caChangesTotal;
    }

    void setOneBudANCMode(bool enabled)
    {
        if (m_deviceInfo->oneBudANCMode() == enabled)
        {
            LOG_DEBUG("One Bud ANC mode is already " << (enabled ? "enabled" : "disabled"));
            return;
        }

        LOG_INFO("Setting One Bud ANC mode to: " << (enabled ? "enabled" : "disabled"));
        QByteArray packet = enabled ? AirPodsPackets::OneBudANCMode::ENABLED
                                    : AirPodsPackets::OneBudANCMode::DISABLED;

        if (writePacketToSocket(packet, "One Bud ANC mode packet written: "))
        {
            m_deviceInfo->setOneBudANCMode(enabled);
            ++m_oneBudANCChangesTotal;
        }
        else
        {
            LOG_ERROR("Failed to send One Bud ANC mode command: socket not open");
        }
    }

    void setRetryAttempts(int attempts)
    {
        if (m_retryAttempts != attempts)
        {
            LOG_DEBUG("Setting retry attempts to: " << attempts);
            m_retryAttempts = attempts;
            emit retryAttemptsChanged(attempts);
            saveRetryAttempts(attempts);
        }
    }

    void initiateMagicPairing()
    {
        if (!socket || !socket->isOpen())
        {
            LOG_ERROR("Socket nicht offen, Magic Pairing kann nicht gestartet werden");
            return;
        }

        writePacketToSocket(AirPodsPackets::MagicPairing::REQUEST_MAGIC_CLOUD_KEYS, "Magic Pairing packet written: ");
    }

    void setAdaptiveNoiseLevel(int level)
    {
        level = qBound(0, level, 100);
        if (m_deviceInfo->adaptiveNoiseLevel() != level && m_deviceInfo->adaptiveModeActive())
        {
            QByteArray packet = AirPodsPackets::AdaptiveNoise::getPacket(level);
            writePacketToSocket(packet, "Adaptive noise level packet written: ");
            m_deviceInfo->setAdaptiveNoiseLevel(level);
            ++m_adaptiveLevelChangesTotal;
        }
    }

    void renameAirPods(const QString &newName)
    {
        if (newName.isEmpty())
        {
            LOG_WARN("Cannot set empty name");
            return;
        }
        if (newName.size() > 32)
        {
            LOG_WARN("Name is too long, must be 32 characters or less");
            return;
        }
        if (newName == m_deviceInfo->deviceName())
        {
            LOG_DEBUG("Name is already set to: " << newName);
            return;
        }

        QByteArray packet = AirPodsPackets::Rename::getPacket(newName);
        if (writePacketToSocket(packet, "Rename packet written: "))
        {
            LOG_INFO("Sent rename command for new name: " << newName);
            m_deviceInfo->setDeviceName(newName);
        }
        else
        {
            LOG_ERROR("Failed to send rename command: socket not open");
        }
    }

    void setEarDetectionBehavior(int behavior)
    {
        if (behavior == earDetectionBehavior())
        {
            LOG_DEBUG("Ear detection behavior is already set to: " << behavior);
            return;
        }

        mediaController->setEarDetectionBehavior(static_cast<MediaController::EarDetectionBehavior>(behavior));
        saveEarDetectionSettings();
        ++m_earDetectionChangesTotal;
        emit earDetectionBehaviorChanged(behavior);
    }

    void setCrossDeviceEnabled(bool enabled)
    {
        if (CrossDevice.isEnabled == enabled)
        {
            LOG_DEBUG("Cross-device feature is already " << (enabled ? "enabled" : "disabled"));
            return;
        }

        CrossDevice.isEnabled = enabled;
        saveCrossDeviceEnabled();
        connectToPhone();
        emit crossDeviceEnabledChanged(enabled);
    }

    void setPhoneMac(const QString &mac)
    {
        if (mac.isEmpty()) {
            LOG_WARN("Empty MAC provided, ignoring");
            m_phoneMacStatus = QStringLiteral("No MAC provided (ignoring)");
            emit phoneMacStatusChanged();
            return;
        }

        // Basic MAC address validation (accepts formats like AA:BB:CC:DD:EE:FF, AABBCCDDEEFF, AA-BB-CC-DD-EE-FF)
        QRegularExpression re("^([0-9A-Fa-f]{2}([-:]?)){5}[0-9A-Fa-f]{2}$");
        if (!re.match(mac).hasMatch()) {
            LOG_ERROR("Invalid MAC address format: " << mac);
            m_phoneMacStatus = QStringLiteral("Invalid MAC: ") + mac;
            emit phoneMacStatusChanged();
            return;
        }

        // Persist via QSettings so restart picks it up; previously only
        // env var was set in-process and was lost on daemon exit. User
        // had to re-enter on every launch.
        if (m_settings) {
            m_settings->setValue(QStringLiteral("phoneMac"), mac);
            m_settings->sync();
        }

        // Set environment variable for the running process
        qputenv("PHONE_MAC_ADDRESS", mac.toUtf8());
        LOG_INFO("PHONE_MAC_ADDRESS environment variable set to: " << mac);

        m_phoneMacStatus = QStringLiteral("Updated MAC: ") + mac;
        emit phoneMacStatusChanged();

        // Update QML context property so UI placeholders reflect the new value
        if (parent) {
            parent->rootContext()->setContextProperty("PHONE_MAC_ADDRESS", mac);
        }

        // If a phone socket exists, restart connection using the new MAC
        if (phoneSocket && phoneSocket->isOpen()) {
            phoneSocket->close();
            phoneSocket->deleteLater();
            phoneSocket = nullptr;
        }
        connectToPhone();
    }

    void updatePhoneMacStatus(const QString &status)
    {
        m_phoneMacStatus = status;
        emit phoneMacStatusChanged();
    }

    void setHearingAidEnabled(bool enabled)
    {
        LOG_INFO("Setting hearing aid to: " << (enabled ? "enabled" : "disabled"));
        QByteArray packet = enabled ? AirPodsPackets::HearingAid::ENABLED
                                    : AirPodsPackets::HearingAid::DISABLED;

        writePacketToSocket(packet, "Hearing aid packet written: ");
        m_deviceInfo->setHearingAidEnabled(enabled);
    }

    bool writePacketToSocket(const QByteArray &packet, const QString &logMessage)
    {
        if (socket && socket->isOpen())
        {
            socket->write(packet);
            LOG_DEBUG(logMessage << packet.toHex());
            return true;
        }
        else
        {
            LOG_ERROR("Socket is not open, cannot write packet");
            return false;
        }
    }

    // The settings file holds the pairing keys, so keep it and its directory owner only.
    void restrictSettingsAccess()
    {
        if (!m_settings) {
            return;
        }
        m_settings->sync();
        const QString file = m_settings->fileName();
        QFile::setPermissions(QFileInfo(file).absolutePath(),
                              QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ExeOwner);
        if (QFile::exists(file)) {
            QFile::setPermissions(file, QFileDevice::ReadOwner | QFileDevice::WriteOwner);
        }
    }

    bool loadCrossDeviceEnabled() { return m_settings->value("crossdevice/enabled", false).toBool(); }
    void saveCrossDeviceEnabled() { m_settings->setValue("crossdevice/enabled", CrossDevice.isEnabled); }

    int loadEarDetectionSettings() { return m_settings->value("earDetection/setting", MediaController::EarDetectionBehavior::PauseWhenOneRemoved).toInt(); }
    void saveEarDetectionSettings() { m_settings->setValue("earDetection/setting", mediaController->getEarDetectionBehavior()); }

    bool loadNotificationsEnabled() const { return m_settings->value("notifications/enabled", true).toBool(); }
    void saveNotificationsEnabled(bool enabled) { m_settings->setValue("notifications/enabled", enabled); }

    int loadRetryAttempts() const { return m_settings->value("bluetooth/retryAttempts", 3).toInt(); }
    void saveRetryAttempts(int attempts) { m_settings->setValue("bluetooth/retryAttempts", attempts); }

    // Only Apple's own controller drops A2DP packets under continuous BLE discovery.
    static bool bluetoothControllerIsApple()
    {
        static const bool affected = []() {
            QDir sys(QStringLiteral("/sys/class/bluetooth"));
            const QStringList adapters = sys.entryList(QStringList() << QStringLiteral("hci*"),
                                                       QDir::Dirs | QDir::NoDotAndDotDot);
            for (const QString &adapter : adapters) {
                QFile modalias(sys.filePath(adapter) + QStringLiteral("/device/modalias"));
                if (!modalias.open(QIODevice::ReadOnly | QIODevice::Text))
                    continue;
                if (AppleController::modaliasIsApple(QString::fromLatin1(modalias.readAll())))
                    return true;
            }
            return false;
        }();
        return affected;
    }

    void stopBleScanWhileConnected()
    {
        // Everywhere else keeps discovery running, so case and lid battery stay live while connected.
        if (!bluetoothControllerIsApple() || !areAirpodsConnected() || !m_bleManager->isScanning())
            return;

        LOG_INFO("Stopping BLE scan while AirPods control link is connected");
        m_bleManager->stopScan();
    }

    void onSystemGoingToSleep()
    {
        m_isSuspending = true;
        if (m_bleManager->isScanning())
        {
            LOG_INFO("Stopping BLE scan before going to sleep");
            m_bleManager->stopScan();
        }
    }
    void onSystemWakingUp()
    {
        LOG_INFO("System wake-up; deferring BLE rediscovery 2s for BlueZ to settle");

        // BlueZ + the kernel BT controller often need ~1-3s after resume
        // before hci0 is responsive again. Firing scan + GetManagedObjects
        // immediately on the PrepareForSleep:false signal races the stack
        // and the first scan typically returns no devices. Wait a beat,
        // and keep m_isSuspending true until the grace window closes so
        // the disconnect notifications that BlueZ fires during resume
        // don't surface as user-visible "AirPods Disconnected" toasts.
        QTimer::singleShot(2000, this, [this]() {
            // Suspend stopped discovery on every controller, so resume restarts it and the gate below re-applies.
            m_bleManager->startScan();

            if (areAirpodsConnected() && m_deviceInfo && !m_deviceInfo->bluetoothAddress().isEmpty())
            {
                stopBleScanWhileConnected();
                LOG_INFO("AirPods already connected after wake-up, re-activating A2DP profile");
                // Profile may have been dropped during suspend; reassert.
                QTimer::singleShot(1000, this, [this]() {
                    LOG_INFO("A2DP profile activation attempted after system wake-up");
                    mediaController->activateA2dpProfileWithRetry(m_deviceInfo->bluetoothAddress().replace(":", "_"));
                });
            }

            monitor->checkAlreadyConnectedDevices();
            m_isSuspending = false;
        });
    }

private slots:
    void onTrayIconActivated()
    {
        // --hide defers the QML load, so a tray click before the first reopen finds no window at all.
        const auto windows = QGuiApplication::topLevelWindows();
        if (windows.isEmpty()) {
            loadMainModule();
            return;
        }
        QQuickWindow *window = qobject_cast<QQuickWindow *>(windows.constFirst());
        if (window)
        {
            window->show();
            window->raise();
            window->requestActivate();
        }
    }

    void onOpenApp()
    {
        if (!parent) return;
        // Guard against empty rootObjects: QList::first() on an empty
        // list is undefined behavior, not a null return. After the
        // lazy-QML deferral in main() (--hide skips eager
        // loadMainModule), rootObjects can be empty until the first
        // reopen — which is exactly when this handler fires.
        const auto roots = parent->rootObjects();
        if (!roots.isEmpty()) {
            QMetaObject::invokeMethod(roots.first(), "reopen", Q_ARG(QVariant, "app"));
        } else {
            loadMainModule();
        }
    }

    void onOpenSettings()
    {
        if (!parent) return;
        const auto roots = parent->rootObjects();
        if (!roots.isEmpty()) {
            QMetaObject::invokeMethod(roots.first(), "reopen", Q_ARG(QVariant, "settings"));
        } else {
            // First-time settings click with no QML loaded yet: just
            // load Main.qml. It opens to the main page by default;
            // user clicks the settings icon to drill in. One extra
            // step on first-launch vs. allocating ~60-100MB of QML
            // overhead eagerly at every daemon start.
            loadMainModule();
        }
    }

    void sendHandshake() {
        LOG_INFO("Connected to device, sending initial packets");
        writePacketToSocket(AirPodsPackets::Connection::HANDSHAKE, "Handshake packet written: ");
    }

    void bluezDeviceConnected(const QString &address, const QString &name)
    {
        rememberAirPodsDevice(address, name);
        m_disconnectFinalized = false;

        QBluetoothDeviceInfo device(QBluetoothAddress(address), name, 0);
        const bool recovering = m_controlRecovery.isActive();
        if (recovering) {
            if (m_controlRecovery.state() == ControlReconnect::State::ConnectingSocket && socket) {
                LOG_DEBUG("Control recovery connection is already in progress");
                return;
            }
            m_controlReconnectTimer->stop();
            m_controlRecovery.beginConnection();
        }
        connectToDevice(device, recovering);

        // After system reboot, AirPods might be connected but A2DP profile not active
        // Attempt to activate A2DP profile after a delay to ensure connection is established
        if (!recovering) QTimer::singleShot(2000, this, [this, address]()
        {
            if (!address.isEmpty())
            {
                QString formattedAddress = address;
                formattedAddress = formattedAddress.replace(":", "_");
                LOG_INFO("A2DP profile activation attempted for newly connected device");
                mediaController->activateA2dpProfileWithRetry(formattedAddress);
            }
        });
    }

    void onDeviceDisconnected(const QBluetoothAddress &address)
    {
        LOG_INFO("Device disconnected: " << address.toString());
        // A retry still in flight would reactivate a profile for the device that just went away.
        mediaController->cancelPendingA2dpActivation();
        if (socket)
        {
            LOG_WARN("Socket is still open, closing it");
            socket->disconnect(this);
            socket->close();
            socket->deleteLater();
            socket = nullptr;
        }

        // BlueZ lowers Connected while rebuilding a profile, so try the control socket before tearing down.
        scheduleControlReconnect(address.toString(), m_lastAirPodsName,
                                 QStringLiteral("BlueZ disconnect event"));
    }

    void finalizeDeviceDisconnected(const QString &address)
    {
        if (m_disconnectFinalized) {
            return;
        }
        m_disconnectFinalized = true;
        m_controlReconnectTimer->stop();
        m_controlRecovery.cancel();
        m_retryCount = 0;

        if (phoneSocket && phoneSocket->isOpen())
        {
            phoneSocket->write(AirPodsPackets::Connection::AIRPODS_DISCONNECTED);
            LOG_DEBUG("AIRPODS_DISCONNECTED packet written: " << AirPodsPackets::Connection::AIRPODS_DISCONNECTED.toHex());
        }

        // Clear the device name and model
        m_deviceInfo->reset();
        m_bleManager->startScan();
        emit airPodsStatusChanged();

        // Skip the disconnect toast if we're suspending/resuming — BlueZ
        // fires PropertiesChanged Connected=false on suspend and the user
        // doesn't want to see "AirPods Disconnected" every time they close
        // the lid. Tray icon still resets so visual state is accurate.
        if (!m_isSuspending) {
            m_notifier->notify(
                tr("AirPods Disconnected"),
                tr("Your AirPods have been disconnected"));
        }
        if (trayManager) {
            trayManager->resetTrayIcon();
        }
    }

    void rememberAirPodsDevice(const QString &address, const QString &name)
    {
        if (!address.isEmpty()) {
            m_lastAirPodsAddress = address;
        }
        if (!name.isEmpty()) {
            m_lastAirPodsName = name;
        }
    }

    void scheduleControlReconnect(const QString &address, const QString &name,
                                  const QString &reason)
    {
        rememberAirPodsDevice(address, name);
        if (m_lastAirPodsAddress.isEmpty() || m_isSuspending) {
            finalizeDeviceDisconnected(address);
            return;
        }

        const bool bleScanWasActive = m_bleManager->isScanning();

        // A live scan delays the L2CAP connect this recovery depends on, so pause it and restore after.
        m_bleManager->stopScan();

        if (!m_controlRecovery.isActive()) {
            m_controlRecovery.begin(bleScanWasActive);
            m_disconnectFinalized = false;
            LOG_INFO("Scheduling AirPods control reconnect after " << reason);
            m_controlReconnectTimer->start(ControlReconnect::firstDelayMs);
        } else if (m_controlRecovery.state() == ControlReconnect::State::ConnectingSocket) {
            scheduleControlRecoveryRetry(reason, false);
        }
    }

    // Runs every ControlReconnect::watchdogIntervalMs. scheduleControlReconnect() only
    // fires from a BlueZ PropertiesChanged transition or a control-socket callback, so once
    // its bounded retry budget (scheduleControlRecoveryRetry -> finalizeDeviceDisconnected)
    // is exhausted, nothing retries again unless one of those events happens to fire - even
    // though the AirPods can still be sitting connected the whole time. This is the fallback
    // that notices and tries again, on a slow enough cadence to stay a safety net rather than
    // a second retry loop racing the bounded one.
    void checkControlLinkWatchdog()
    {
        if (areAirpodsConnected() || m_controlRecovery.isActive() || m_isSuspending) {
            return;
        }
        if (m_lastAirPodsAddress.isEmpty()) {
            return;
        }

        LOG_INFO("Control link watchdog: rechecking BlueZ connection state for "
                 << m_lastAirPodsAddress);
        scheduleControlReconnect(m_lastAirPodsAddress, m_lastAirPodsName,
                                 QStringLiteral("watchdog recheck"));
    }

    void attemptControlReconnect()
    {
        if (m_controlRecovery.state() != ControlReconnect::State::Waiting) {
            return;
        }

        if (areAirpodsConnected()) {
            finishControlRecovery();
            return;
        }

        const QString address = m_lastAirPodsAddress;
        if (address.isEmpty()) {
            finalizeDeviceDisconnected(address);
            return;
        }

        const quint64 requestId = m_controlRecovery.beginProbe();
        monitor->probeDeviceConnected(address, requestId);
    }

    void controlConnectionProbeFinished(const QString &address, quint64 requestId,
                                        bool connected)
    {
        if (address.compare(m_lastAirPodsAddress, Qt::CaseInsensitive) != 0
            || !m_controlRecovery.acceptsProbe(requestId)) {
            LOG_DEBUG("Ignoring stale AirPods connection probe");
            return;
        }

        if (connected) {
            ++m_reconnectAttemptsTotal;
            LOG_INFO("Reconnecting AirPods control link (attempt total="
                     << m_reconnectAttemptsTotal << ")");
            m_controlRecovery.beginConnection();
            QBluetoothDeviceInfo device(QBluetoothAddress(address), m_lastAirPodsName, 0);
            connectToDevice(device, true);
            return;
        }

        scheduleControlRecoveryRetry(QStringLiteral("BlueZ did not report the device as connected"), false);
    }

    // deviceStillConnected picks the longer ladder, because BlueZ saying it has the device means the endpoint is there.
    void scheduleControlRecoveryRetry(const QString &reason, bool deviceStillConnected)
    {
        if (m_controlRecovery.prepareRetry(m_retryAttempts, deviceStillConnected)) {
            const int jitter = static_cast<int>(QRandomGenerator::global()->bounded(ControlReconnect::jitterRangeMs));
            const int spent = deviceStillConnected ? m_controlRecovery.connectedAttempts()
                                                   : m_controlRecovery.absentProbes();
            const int limit = deviceStillConnected ? ControlReconnect::connectedAttemptLimit
                                                   : m_retryAttempts;
            // Each branch backs off on its own count, so the one number in the line explains the delay beside it.
            const int delay = ControlReconnect::delayMs(spent, jitter);
            LOG_INFO("Retrying AirPods control recovery after " << reason << " ("
                     << spent << "/" << limit << ", delay=" << delay << "ms)");
            m_controlReconnectTimer->start(delay);
            return;
        }

        ++m_reconnectFailuresTotal;
        if (deviceStillConnected) {
            LOG_WARN("AirPods never accepted the control socket while BlueZ held "
                     << m_lastAirPodsAddress << ", finalizing");
        } else {
            LOG_INFO("AirPods stayed disconnected through control recovery, finalizing "
                     << m_lastAirPodsAddress);
        }
        finalizeDeviceDisconnected(m_lastAirPodsAddress);
    }

    void finishControlRecovery()
    {
        if (!m_controlRecovery.isActive()) {
            return;
        }

        m_controlReconnectTimer->stop();
        const bool restoreBleScan = m_controlRecovery.complete();
        m_disconnectFinalized = false;
        if (restoreBleScan) {
            m_bleManager->startScan();
        } else {
            m_bleManager->stopScan();
        }
        LOG_INFO("AirPods control link recovered");
    }

    void bluezDeviceDisconnected(const QString &address, const QString &name)
    {
        if (address == m_deviceInfo->bluetoothAddress())
        {
            onDeviceDisconnected(QBluetoothAddress(address));
        } else {
            LOG_WARN("Disconnected device does not match connected device: " << address << " != " << m_deviceInfo->bluetoothAddress());
        }
    }

    void parseMetadata(const QByteArray &data)
    {
        // Verify the data starts with the METADATA header
        if (!data.startsWith(AirPodsPackets::Parse::METADATA))
        {
            LOG_ERROR("Invalid metadata packet: Incorrect header");
            return;
        }

        int pos = AirPodsPackets::Parse::METADATA.size(); // Start after the header

        // Check if there is enough data to skip the initial bytes (based on example structure)
        if (data.size() < pos + 6)
        {
            LOG_ERROR("Metadata packet too short to parse initial bytes");
            return;
        }
        pos += 6; // Skip 6 bytes after the header as per example structure

        auto extractString = [&data, &pos]() -> QString
        {
            if (pos >= data.size())
            {
                return QString();
            }
            int start = pos;
            while (pos < data.size() && data.at(pos) != '\0')
            {
                ++pos;
            }
            QString str = QString::fromUtf8(data.mid(start, pos - start));
            if (pos < data.size())
            {
                ++pos; // Move past the null terminator
            }
            return str;
        };

        m_deviceInfo->setDeviceName(extractString());
        m_deviceInfo->setModelNumber(extractString());
        m_deviceInfo->setManufacturer(extractString());

        m_deviceInfo->setModel(parseModelNumber(m_deviceInfo->modelNumber()));
        emit modelChanged();

        // Persist the parsed metadata so loadFromSettings can re-run
        // parseModelNumber on next startup (lets newly-added enum map
        // entries like AirPodsPro3 A3064 resolve without waiting for
        // AAP metadata to re-fire).
        if (m_settings) {
            m_deviceInfo->saveToSettings(*m_settings);
            restrictSettingsAccess();
        }

        // Log extracted metadata
        LOG_INFO("Parsed AirPods metadata:");
        LOG_INFO("Device Name: " << m_deviceInfo->deviceName());
        LOG_INFO("Model Number: " << m_deviceInfo->modelNumber());
        LOG_INFO("Manufacturer: " << m_deviceInfo->manufacturer());
    }

    QString getEarStatus(char value)
    {
        return (value == 0x00) ? "In Ear" : (value == 0x01) ? "Out of Ear"
                                                            : "In case";
    }

    void connectToDevice(const QBluetoothDeviceInfo &device, bool controlRecovery = false)
    {
        if (socket && socket->isOpen() && socket->peerAddress() == device.address())
        {
            LOG_INFO("Already connected to the device: " << device.name());
            return;
        }

        LOG_INFO("Connecting to device: " << device.name());
        rememberAirPodsDevice(device.address().toString(), device.name());

        // Clean up any existing socket (defensive: disconnect first so any
        // queued slot can't reach a half-dead object).
        if (socket) {
            socket->disconnect(this);
            socket->close();
            socket->deleteLater();
            socket = nullptr;
        }

        QBluetoothSocket *localSocket = new QBluetoothSocket(QBluetoothServiceInfo::L2capProtocol, this);
        localSocket->setProperty("openpodsControlRecovery", controlRecovery);
        socket = localSocket;

        // Connection handler
        auto handleConnection = [this, localSocket]()
        {
            localSocket->setProperty("openpodsControlConnected", true);
            m_retryCount = 0;
            m_disconnectFinalized = false;
            if (localSocket->property("openpodsControlRecovery").toBool()) {
                finishControlRecovery();
            }
            stopBleScanWhileConnected();
            connect(localSocket, &QBluetoothSocket::readyRead, this, [this, localSocket]()
                    {
            QByteArray data = localSocket->readAll();
            QMetaObject::invokeMethod(this, "parseData", Qt::QueuedConnection, Q_ARG(QByteArray, data));
            QMetaObject::invokeMethod(this, "relayPacketToPhone", Qt::QueuedConnection, Q_ARG(QByteArray, data)); });
            sendHandshake();
        };

        // Error handler with retry. Per-device member instead of static so
        // attempts don't leak across devices (e.g. switching between two
        // paired AirPods).
        auto handleError = [this, device, localSocket](QBluetoothSocket::SocketError error)
        {
            LOG_ERROR("Socket error: " << error << ", " << localSocket->errorString());

            // Once a control link has been established, a later socket error
            // is a link-loss event, not another initial-connect failure.
            if (localSocket->property("openpodsControlConnected").toBool()) {
                handleControlSocketLoss(device, localSocket,
                                        QStringLiteral("socket error"));
                return;
            }

            if (localSocket->property("openpodsControlRecovery").toBool()) {
                handleControlConnectFailure(device, localSocket,
                                            QStringLiteral("socket error before connection"));
                return;
            }

            if (m_retryCount < m_retryAttempts)
            {
                m_retryCount++;
                m_reconnectAttemptsTotal++;
                // Exponential backoff with jitter: 1000 * 2^(n-1) + rand(0..499)ms.
                // n=1 -> ~1.0-1.5s, n=2 -> ~2.0-2.5s, n=3 -> ~4.0-4.5s.
                // Fixed-delay 1500ms hammered BlueZ if multiple peers were
                // also retrying after a controller reset — exponential
                // spacing + jitter avoids the thundering-herd reconnect.
                const int jitter = static_cast<int>(QRandomGenerator::global()->bounded(ControlReconnect::jitterRangeMs));
                const int delay = ControlReconnect::delayMs(m_retryCount, jitter);
                LOG_INFO("Retrying connection (attempt " << m_retryCount << "/" << m_retryAttempts
                         << ", delay=" << delay << "ms, total=" << m_reconnectAttemptsTotal << ")");
                QTimer::singleShot(delay, this, [this, device]()
                                   { connectToDevice(device); });
            }
            else
            {
                m_reconnectFailuresTotal++;
                LOG_ERROR("Failed to connect after " << m_retryAttempts
                          << " attempts (total failures=" << m_reconnectFailuresTotal << ")");
                m_retryCount = 0;
                // BlueZ never raises Connected again for a device it already holds, so hand over to the probe ladder.
                scheduleControlReconnect(device.address().toString(), device.name(),
                                         QStringLiteral("initial connect attempts exhausted"));
            }
        };

        connect(localSocket, &QBluetoothSocket::connected, this, handleConnection);
        connect(localSocket, QOverload<QBluetoothSocket::SocketError>::of(&QBluetoothSocket::errorOccurred),
                this, handleError);
        connect(localSocket, &QBluetoothSocket::disconnected, this,
                [this, device, localSocket]() {
                    if (!localSocket->property("openpodsControlConnected").toBool()) {
                        if (localSocket->property("openpodsControlRecovery").toBool()) {
                            handleControlConnectFailure(
                                device, localSocket,
                                QStringLiteral("socket disconnected before connection"));
                        }
                        return;
                    }
                    handleControlSocketLoss(device, localSocket,
                                            QStringLiteral("socket disconnected"));
                });

        localSocket->connectToService(device.address(), QBluetoothUuid("74ec2172-0bad-4d01-8f77-997b2be0722a"));
        m_deviceInfo->setBluetoothAddress(device.address().toString());
        notifyAndroidDevice();
    }

    void handleControlConnectFailure(const QBluetoothDeviceInfo &device,
                                     QBluetoothSocket *failedSocket,
                                     const QString &reason)
    {
        if (!failedSocket || socket != failedSocket || !m_controlRecovery.isActive()) {
            return;
        }

        LOG_WARN("AirPods control reconnect failed: " << reason);
        failedSocket->disconnect(this);
        failedSocket->close();
        failedSocket->deleteLater();
        socket = nullptr;
        rememberAirPodsDevice(device.address().toString(), device.name());
        scheduleControlRecoveryRetry(reason, true);
    }

    void handleControlSocketLoss(const QBluetoothDeviceInfo &device,
                                 QBluetoothSocket *lostSocket,
                                 const QString &reason)
    {
        // errorOccurred and disconnected commonly arrive for the same loss;
        // only the first callback for the currently-owned socket may recover.
        if (!lostSocket || socket != lostSocket) {
            return;
        }

        LOG_WARN("AirPods control link lost: " << reason);
        lostSocket->disconnect(this);
        lostSocket->close();
        lostSocket->deleteLater();
        socket = nullptr;

        scheduleControlReconnect(device.address().toString(), device.name(), reason);
    }

    void parseData(const QByteArray &data)
    {
        LOG_DEBUG("Received: " << data.toHex());

        if (data.startsWith(AirPodsPackets::Parse::HANDSHAKE_ACK))
        {
            writePacketToSocket(AirPodsPackets::Connection::SET_SPECIFIC_FEATURES, "Set specific features packet written: ");
        }
        else if (data.startsWith(AirPodsPackets::Parse::FEATURES_ACK))
        {
            writePacketToSocket(AirPodsPackets::Connection::REQUEST_NOTIFICATIONS, "Request notifications packet written: ");

            QTimer::singleShot(2000, this, [this]() {
                if (m_deviceInfo->batteryStatus().isEmpty()) {
                    writePacketToSocket(AirPodsPackets::Connection::REQUEST_NOTIFICATIONS, "Request notifications packet written: ");
                }
            });
        }
        // Magic Cloud Keys Response
        else if (data.startsWith(AirPodsPackets::MagicPairing::MAGIC_CLOUD_KEYS_HEADER))
        {
            auto keys = AirPodsPackets::MagicPairing::parseMagicCloudKeysPacket(data);
            // Sizes, never the key material: the journal is not where a pairing key belongs.
            LOG_INFO("Received Magic Cloud Keys: IRK " << keys.magicAccIRK.size()
                     << " bytes, EncKey " << keys.magicAccEncKey.size() << " bytes");

            // Store the keys
            m_deviceInfo->setMagicAccIRK(keys.magicAccIRK);
            m_deviceInfo->setMagicAccEncKey(keys.magicAccEncKey);
            m_deviceInfo->saveToSettings(*m_settings);
            restrictSettingsAccess();
        }
        // Get CA state
        else if (data.startsWith(AirPodsPackets::ConversationalAwareness::HEADER)) {
            if (auto result = AirPodsPackets::ConversationalAwareness::parseState(data))
            {
                m_deviceInfo->setConversationalAwareness(result.value());
                LOG_DEBUG("Conversational awareness state received: " << m_deviceInfo->conversationalAwareness());
            }
        }
        // Hearing Aid state
        else if (data.startsWith(AirPodsPackets::HearingAid::HEADER)) {
            if (auto result = AirPodsPackets::HearingAid::parseState(data))
            {
                m_deviceInfo->setHearingAidEnabled(result.value());
                LOG_DEBUG("Hearing aid state received: " << m_deviceInfo->hearingAidEnabled());
            }
        }
        // Noise Control Mode
        else if (data.size() == 11 && data.startsWith(AirPodsPackets::NoiseControl::HEADER))
        {
            if (auto value = AirPodsPackets::NoiseControl::parseMode(data))
            {
                m_deviceInfo->setNoiseControlMode(value.value());
                LOG_DEBUG("Noise control mode received: " << m_deviceInfo->noiseControlMode());
            }
        }
        // Ear Detection
        else if (data.size() == 8 && data.startsWith(AirPodsPackets::Parse::EAR_DETECTION))
        {
            m_deviceInfo->getEarDetection()->parseData(data);
            mediaController->handleEarDetection(m_deviceInfo->getEarDetection());
        }
        // Battery Status
        else if ((data.size() == 22 || data.size() == 12) && data.startsWith(AirPodsPackets::Parse::BATTERY_STATUS))
        {
            m_deviceInfo->getBattery()->parsePacket(data);
            m_deviceInfo->updateBatteryStatus();
            LOG_INFO("Battery status: " << m_deviceInfo->batteryStatus());
        }
        // Conversational Awareness Data
        else if (data.size() == 10 && data.startsWith(AirPodsPackets::ConversationalAwareness::DATA_HEADER))
        {
            LOG_INFO("Received conversational awareness data");
            mediaController->handleConversationalAwareness(data);
        }
        else if (data.startsWith(AirPodsPackets::Parse::METADATA))
        {
            parseMetadata(data);
            initiateMagicPairing();
            if (m_deviceInfo->getEarDetection()->oneOrMorePodsInEar()) // AirPods get added as output device only after this
            {
                mediaController->activateA2dpProfileWithRetry(m_deviceInfo->bluetoothAddress().replace(":", "_"));
            }
            else
            {
                mediaController->setConnectedDeviceMacAddress(m_deviceInfo->bluetoothAddress().replace(":", "_"));
            }
            // Only on the affected controller, where onDeviceDisconnected restarts it.
            stopBleScanWhileConnected();
            emit airPodsStatusChanged();
        }
        else if (data.startsWith(AirPodsPackets::OneBudANCMode::HEADER)) {
            if (auto value = AirPodsPackets::OneBudANCMode::parseState(data))
            {
                m_deviceInfo->setOneBudANCMode(value.value());
                LOG_DEBUG("One Bud ANC mode received: " << m_deviceInfo->oneBudANCMode());
            }
        }
        else
        {
            LOG_DEBUG("Unrecognized packet format: " << data.toHex());
        }
    }

    void connectToPhone() {
        if (!CrossDevice.isEnabled) {
            return;
        }

        if (phoneSocket && phoneSocket->isOpen()) {
            LOG_INFO("Already connected to the phone");
            return;
        }
        // If a previous attempt left a non-open socket lying around (e.g.
        // mid-connect or error state), drop it before allocating a new one
        // — otherwise we leak a QBluetoothSocket per call.
        if (phoneSocket) {
            phoneSocket->disconnect(this);
            phoneSocket->abort();
            phoneSocket->deleteLater();
            phoneSocket = nullptr;
        }

        QBluetoothAddress phoneAddress("00:00:00:00:00:00"); // Default address, will be overwritten if PHONE_MAC_ADDRESS is set
        QProcessEnvironment env = QProcessEnvironment::systemEnvironment();

        if (!env.value("PHONE_MAC_ADDRESS").isEmpty())
        {
            phoneAddress = QBluetoothAddress(env.value("PHONE_MAC_ADDRESS"));
        }
        phoneSocket = new QBluetoothSocket(QBluetoothServiceInfo::L2capProtocol, this);
        connect(phoneSocket, &QBluetoothSocket::connected, this, [this]() {
            LOG_INFO("Connected to phone");
            if (!lastBatteryStatus.isEmpty()) {
                phoneSocket->write(lastBatteryStatus);
                LOG_DEBUG("Sent last battery status to phone: " << lastBatteryStatus.toHex());
            }
            if (!lastEarDetectionStatus.isEmpty()) {
                phoneSocket->write(lastEarDetectionStatus);
                LOG_DEBUG("Sent last ear detection status to phone: " << lastEarDetectionStatus.toHex());
            }
        });

        connect(phoneSocket, QOverload<QBluetoothSocket::SocketError>::of(&QBluetoothSocket::errorOccurred), this, [this](QBluetoothSocket::SocketError error) {
            LOG_ERROR("Phone socket error: " << error << ", " << phoneSocket->errorString());
        });

        phoneSocket->connectToService(phoneAddress, QBluetoothUuid("1abbb9a4-10e4-4000-a75c-8953c5471342"));
    }

    void relayPacketToPhone(const QByteArray &packet)
    {
        if (!CrossDevice.isEnabled) {
            return;
        }
        if (phoneSocket && phoneSocket->isOpen())
        {
            phoneSocket->write(AirPodsPackets::Phone::NOTIFICATION + packet);
        }
        else
        {
            connectToPhone();
            LOG_WARN("Phone socket is not open, cannot relay packet");
        }
    }

    void handlePhonePacket(const QByteArray &packet) {
        if (packet.startsWith(AirPodsPackets::Phone::NOTIFICATION))
        {
            QByteArray airpodsPacket = packet.mid(4);
            if (socket && socket->isOpen()) {
                socket->write(airpodsPacket);
                LOG_DEBUG("Relayed packet to AirPods: " << airpodsPacket.toHex());
            } else {
                LOG_ERROR("Socket is not open, cannot relay packet to AirPods");
            }
        }
        else if (packet.startsWith(AirPodsPackets::Phone::CONNECTED))
        {
            LOG_INFO("AirPods connected");
            isConnectedLocally = true;
            CrossDevice.isAvailable = false;
        }
        else if (packet.startsWith(AirPodsPackets::Phone::DISCONNECTED))
        {
            LOG_INFO("AirPods disconnected");
            isConnectedLocally = false;
            CrossDevice.isAvailable = true;
        }
        else if (packet.startsWith(AirPodsPackets::Phone::STATUS_REQUEST))
        {
            LOG_INFO("Connection status request received");
            QByteArray response = (socket && socket->isOpen()) ? AirPodsPackets::Phone::CONNECTED
                                                               : AirPodsPackets::Phone::DISCONNECTED;
            phoneSocket->write(response);
            LOG_DEBUG("Sent connection status response: " << response.toHex());
        }
        else if (packet.startsWith(AirPodsPackets::Phone::DISCONNECT_REQUEST))
        {
            LOG_INFO("Disconnect request received");
            if (socket && socket->isOpen()) {
                socket->close();
                LOG_INFO("Disconnected from AirPods");
                // Async: don't block UI thread on bluetoothctl IO.
                auto *proc = new QProcess(this);
                connect(proc, QOverload<int, QProcess::ExitStatus>::of(&QProcess::finished),
                        proc, [proc](int, QProcess::ExitStatus) {
                            LOG_INFO("Bluetoothctl disconnect output: " << proc->readAllStandardOutput().trimmed());
                            proc->deleteLater();
                        });
                proc->start("bluetoothctl", QStringList() << "disconnect" << m_deviceInfo->bluetoothAddress());
                isConnectedLocally = false;
                CrossDevice.isAvailable = true;
            }
        }
        else
        {
            if (socket && socket->isOpen()) {
                socket->write(packet);
                LOG_DEBUG("Relayed packet to AirPods: " << packet.toHex());
            } else {
                LOG_ERROR("Socket is not open, cannot relay packet to AirPods");
            }
        }
    }

    void onPhoneDataReceived() {
        QByteArray data = phoneSocket->readAll();
        LOG_DEBUG("Data received from phone: " << data.toHex());
        QMetaObject::invokeMethod(this, "handlePhonePacket", Qt::QueuedConnection, Q_ARG(QByteArray, data));
    }

    void bleDeviceFound(const BleInfo &device)
    {
        if (BLEUtils::isValidIrkRpa(m_deviceInfo->magicAccIRK(), device.address)) {
            LOG_DEBUG("BLE adv accepted caseBattery=" << device.caseBattery << " charge=" << device.caseCharging << " primaryLeft=" << device.primaryLeft);
            // Only adopt the BLE-broadcast model when it's actually
            // recognized. AirPods Pro 3 (and any future model whose
            // manufacturer-data fingerprint hasn't been added to
            // ble/blemanager.cpp's modelMap yet) reports Unknown here,
            // which would otherwise clobber a valid model from the
            // AAP metadata packet on the L2CAP side.
            if (device.modelName != AirPodsModel::Unknown) {
                m_deviceInfo->setModel(device.modelName);
            }
            auto decryptet = BLEUtils::decryptLastBytes(device.encryptedPayload, m_deviceInfo->magicAccEncKey());
            m_deviceInfo->getBattery()->parseEncryptedPacket(decryptet, device.primaryLeft, device.isThisPodInTheCase, isModelHeadset(m_deviceInfo->model()));
            // Case battery isn't covered by parseEncryptedPacket when
            // pods are out of the case (podInCase=false), so feed the
            // BLE-broadcast case nibble in directly. device.caseBattery
            // is -1 when the broadcast nibble was 15 (unknown); the
            // setter skips those so we don't clobber a valid prior
            // reading.
            // A Max has no case, and its nibble decodes to 0 rather than the 15 that means unknown.
            if (!isModelHeadset(m_deviceInfo->model())) {
                m_deviceInfo->getBattery()->setCaseFromBle(device.caseBattery, device.caseCharging);
            }
            m_deviceInfo->getEarDetection()->overrideEarDetectionStatus(device.isPrimaryInEar, device.isSecondaryInEar);
            m_lidState = device.lidState;
        }
    }

public:
    void handleMediaStateChange(MediaController::MediaState state) {
        // Grabbing the pods off whatever holds them is the cross-device feature, not a
        // side effect of local playback: without this gate the box fights Apple's own
        // automatic switching and the pods flap between here and the other device.
        if (state == MediaController::MediaState::Playing && CrossDevice.isEnabled) {
            LOG_INFO("Media started playing, taking over audio for cross-device");
            sendDisconnectRequestToAndroid();
            connectToAirPods(true);
        }
    }

    void sendDisconnectRequestToAndroid()
    {
        if (!CrossDevice.isEnabled) return;

        if (phoneSocket && phoneSocket->isOpen())
        {
            phoneSocket->write(AirPodsPackets::Phone::DISCONNECT_REQUEST);
            LOG_DEBUG("Sent disconnect request to Android: " << AirPodsPackets::Phone::DISCONNECT_REQUEST.toHex());
        }
        else
        {
            LOG_WARN("Phone socket is not open, cannot send disconnect request");
        }
    }

    bool isPhoneConnected() {
        return phoneSocket && phoneSocket->isOpen();
    }

    void connectToAirPods(bool force) {
        if (socket && socket->isOpen()) {
            LOG_INFO("Already connected to AirPods");
            return;
        }

        if (force) {
            LOG_INFO("Forcing connection to AirPods");
            // Async: don't block UI on bluetoothctl. Once it finishes, walk
            // the connected-device list and dispatch to connectToDevice().
            auto *proc = new QProcess(this);
            connect(proc, QOverload<int, QProcess::ExitStatus>::of(&QProcess::finished),
                    proc, [this, proc](int, QProcess::ExitStatus) {
                        LOG_INFO("Bluetoothctl connect output: " << proc->readAllStandardOutput().trimmed());
                        proc->deleteLater();
                        QBluetoothLocalDevice localDevice;
                        const QList<QBluetoothAddress> connectedDevices = localDevice.connectedDevices();
                        for (const QBluetoothAddress &address : connectedDevices) {
                            QBluetoothDeviceInfo device(address, "", 0);
                            if (isAirPodsDevice(device)) {
                                connectToDevice(device);
                                return;
                            }
                        }
                    });
            proc->start("bluetoothctl", QStringList() << "connect" << m_deviceInfo->bluetoothAddress());
            return;
        }
        QBluetoothLocalDevice localDevice;
        const QList<QBluetoothAddress> connectedDevices = localDevice.connectedDevices();
        for (const QBluetoothAddress &address : connectedDevices) {
            QBluetoothDeviceInfo device(address, "", 0);
            LOG_DEBUG("Connected device: " << device.name() << " (" << device.address().toString() << ")");
            if (isAirPodsDevice(device)) {
                connectToDevice(device);
                return;
            }
        }
        LOG_WARN("AirPods not found among connected devices");
    }

    void initializeBluetooth() {
        connectToPhone();

        m_deviceInfo->loadFromSettings(*m_settings);
        // Nothing is scanning yet, so the affected controller is the only reason not to start.
        if (!(areAirpodsConnected() && bluetoothControllerIsApple()))
            m_bleManager->startScan();
    }

    // Null engine is the headless run, where there is no window to open and nothing to say about it.
    void loadMainModule() {
        if (!parent) {
            LOG_INFO("Running headless, so there is no window to open");
            return;
        }
        parent->load(QUrl(QStringLiteral("qrc:/linux/Main.qml")));
    }

signals:
    void noiseControlModeChanged(NoiseControlMode mode);
    void earDetectionStatusChanged(const QString &status);
    void batteryStatusChanged(const QString &status);
    void conversationalAwarenessChanged(bool enabled);
    void adaptiveNoiseLevelChanged(int level);
    void deviceNameChanged(const QString &name);
    void modelChanged();
    void primaryChanged();
    void airPodsStatusChanged();
    void earDetectionBehaviorChanged(int behavior);
    void crossDeviceEnabledChanged(bool enabled);
    void notificationsEnabledChanged(bool enabled);
    void retryAttemptsChanged(int attempts);
    void oneBudANCModeChanged(bool enabled);
    void phoneMacStatusChanged();
    void hearingAidEnabledChanged(bool enabled);

private:
    QBluetoothSocket *socket = nullptr;
    QBluetoothSocket *phoneSocket = nullptr;
    QByteArray lastBatteryStatus;
    QByteArray lastEarDetectionStatus;
    MediaController* mediaController;
    // Null in a headless run; every use needs a guard.
    TrayIconManager *trayManager = nullptr;
    BluetoothMonitor *monitor;
    QTimer *m_controlReconnectTimer = nullptr;
    // Safety net behind m_controlReconnectTimer's bounded retries; see checkControlLinkWatchdog().
    QTimer *m_controlWatchdogTimer = nullptr;
    QSettings *m_settings;
    AutoStartManager *m_autoStartManager;
    int m_retryAttempts = 3;
    int m_retryCount = 0;
    ControlReconnect::Session m_controlRecovery;
    bool m_isSuspending = false;
    bool m_disconnectFinalized = false;
    QString m_lastAirPodsAddress;
    QString m_lastAirPodsName;

    // Reliability counters — process-lifetime totals, logged on quit.
    // Exposed to QML readers via the public getters below.
    int m_reconnectAttemptsTotal = 0;
    int m_reconnectFailuresTotal = 0;
    // User-action counters. Increment AFTER the no-op short-circuit
    // (so noise:cycle on an already-correct mode doesn't inflate the
    // count) — gives a faithful signal of how often the user is
    // actually toggling. Useful for spotting wedged-mode bugs where
    // setNoiseControlMode is called repeatedly with the same value.
    int m_noiseControlChangesTotal = 0;
    int m_forgetCallsTotal = 0;
    int m_earDetectionChangesTotal = 0;
    int m_caChangesTotal = 0;
    int m_disconnectCallsTotal = 0;
    int m_connectCallsTotal = 0;
    int m_disconnectFailuresTotal = 0;
    int m_connectFailuresTotal = 0;
    int m_adaptiveLevelChangesTotal = 0;
    int m_oneBudANCChangesTotal = 0;
    int m_reopenCallsTotal = 0;

    // Low-battery notification latches per battery source. State +
    // hysteresis live in LowBatteryLatch (see lowbatterywatcher.hpp);
    // tst_lowbatterywatcher.cpp exhaustively covers the trip/reset
    // transitions so this slot only needs to feed the three sources
    // through their respective latches and format the message.
    LowBatteryLatch m_lowBatteryLatchLeft;
    LowBatteryLatch m_lowBatteryLatchRight;
    LowBatteryLatch m_lowBatteryLatchCase;

public:
    int reconnectAttemptsTotal() const { return m_reconnectAttemptsTotal; }
    int reconnectFailuresTotal() const { return m_reconnectFailuresTotal; }
    int noiseControlChangesTotal() const { return m_noiseControlChangesTotal; }
    int forgetCallsTotal() const { return m_forgetCallsTotal; }
    int earDetectionChangesTotal() const { return m_earDetectionChangesTotal; }
    int caChangesTotal() const { return m_caChangesTotal; }
    int disconnectCallsTotal() const { return m_disconnectCallsTotal; }
    int connectCallsTotal() const { return m_connectCallsTotal; }
    int disconnectFailuresTotal() const { return m_disconnectFailuresTotal; }
    int connectFailuresTotal() const { return m_connectFailuresTotal; }
    int adaptiveLevelChangesTotal() const { return m_adaptiveLevelChangesTotal; }
    int oneBudANCChangesTotal() const { return m_oneBudANCChangesTotal; }
    int reopenCallsTotal() const { return m_reopenCallsTotal; }
    void incReopenCallsTotal() { ++m_reopenCallsTotal; }

    // The status snapshot, shared by the IPC verb and the state file so the two
    // can never drift.
    QJsonObject statusJson()
    {
        DeviceInfo *d = deviceInfo();
        Battery *b = d ? d->getBattery() : nullptr;
        QJsonObject status;
        status.insert("schema_version", 1);
        status.insert("connected", areAirpodsConnected());
        status.insert("device_name", d ? d->deviceName() : QString());
        status.insert("noise_mode", d ? d->noiseControlModeInt() : -1);
        if (b) {
            auto pod = [&](bool avail, int level, bool charging, bool inEar) {
                QJsonObject o;
                o.insert("available", avail);
                o.insert("level", level);
                o.insert("charging", charging);
                o.insert("in_ear", inEar);
                return o;
            };
            status.insert("left",  pod(b->isLeftPodAvailable(),  b->getLeftPodLevel(),
                                       b->isLeftPodCharging(),   d->isLeftPodInEar()));
            status.insert("right", pod(b->isRightPodAvailable(), b->getRightPodLevel(),
                                       b->isRightPodCharging(),  d->isRightPodInEar()));
            QJsonObject caseObj;
            caseObj.insert("available", b->isCaseAvailable());
            caseObj.insert("level",    b->getCaseLevel());
            caseObj.insert("charging", b->isCaseCharging());
            status.insert("case", caseObj);
            // A Max reports one battery in Component::Headset, which left, right and case cannot express.
            QJsonObject headsetObj;
            headsetObj.insert("available", b->isHeadsetAvailable());
            headsetObj.insert("level",    b->getHeadsetLevel());
            headsetObj.insert("charging", b->isHeadsetCharging());
            status.insert("headset", headsetObj);
        }
        status.insert("reconnect_attempts_total", reconnectAttemptsTotal());
        status.insert("reconnect_failures_total", reconnectFailuresTotal());
        status.insert("noise_control_changes_total", noiseControlChangesTotal());
        status.insert("forget_calls_total", forgetCallsTotal());
        status.insert("ear_detection_changes_total", earDetectionChangesTotal());
        status.insert("ca_changes_total", caChangesTotal());
        status.insert("disconnect_calls_total", disconnectCallsTotal());
        status.insert("connect_calls_total", connectCallsTotal());
        status.insert("disconnect_failures_total", disconnectFailuresTotal());
        status.insert("connect_failures_total", connectFailuresTotal());
        status.insert("adaptive_level_changes_total", adaptiveLevelChangesTotal());
        status.insert("one_bud_anc_changes_total", oneBudANCChangesTotal());
        status.insert("reopen_calls_total", reopenCallsTotal());
        status.insert("conversational_awareness", d ? d->conversationalAwareness() : false);
        status.insert("adaptive_noise_level", d ? d->adaptiveNoiseLevel() : 0);
        status.insert("one_bud_anc_mode", d ? d->oneBudANCMode() : false);
        status.insert("model_name", d ? modelDisplayName(d->model()) : QString());
        status.insert("model_int", d ? static_cast<int>(d->model()) : 0);
        status.insert("is_pro_series", d ? isProSeriesAirPods(d->model()) : false);
        // The panel needs the shape before any battery packet has arrived.
        status.insert("is_headset", d ? isModelHeadset(d->model()) : false);
        status.insert("supports_noise_off", d ? supportsNoiseOff(d->model()) : true);
        // Additive keys: a panel that reads none of them keeps its is_pro_series behaviour.
        status.insert("supports_noise_control", d ? supportsNoiseControl(d->model()) : true);
        status.insert("supports_adaptive", d ? supportsAdaptiveAudio(d->model()) : false);
        status.insert("supports_conversational_awareness", d ? supportsConversationalAwareness(d->model()) : false);
        status.insert("supports_one_bud_anc", d ? supportsOneBudANC(d->model()) : false);
        // Raw "A<NNNN>" code from the AAP metadata packet. Useful
        // for debugging new/unrecognized models — if a fresh
        // device sends a code that isn't in parseModelNumber's
        // map, model_name will be empty but model_number stays
        // populated so the user can file the missing variant.
        status.insert("model_number", d ? d->modelNumber() : QString());
        // 0 = PauseWhenOneRemoved, 1 = PauseWhenBothRemoved,
        // 2 = Disabled (matches MediaController::EarDetectionBehavior).
        status.insert("ear_detection_behavior", earDetectionBehavior());
        // 0 = open, 1 = closed, 2 = unknown (matches BleInfo::LidState).
        status.insert("lid_state", lidState());
        return status;
    }

    // Published on change for consumers that watch a file instead of polling a
    // process. QSaveFile renames into place, so a reader never sees half a line.
    void writeStateFile()
    {
        const QByteArray line = QJsonDocument(statusJson()).toJson(QJsonDocument::Compact) + "\n";
        if (line == m_lastState) {
            return;
        }
        const QString dir = QStandardPaths::writableLocation(QStandardPaths::GenericStateLocation)
            + QStringLiteral("/librepods");
        if (!QDir().mkpath(dir)) {
            LOG_ERROR("Cannot create state directory: " << dir);
            return;
        }
        QSaveFile file(dir + QStringLiteral("/status.json"));
        if (!file.open(QIODevice::WriteOnly)) {
            LOG_ERROR("Cannot open state file: " << file.fileName());
            return;
        }
        file.write(line);
        if (file.commit()) {
            m_lastState = line;
        }
    }

    // 0 open, 1 closed, 2 unknown, matching BleInfo::LidState.
    int lidState() const { return static_cast<int>(m_lidState); }
private:
    bool m_hideOnStart = false;
    QByteArray m_lastState;
    // Lid only moves on a BLE advertisement, so this stays UNKNOWN until one arrives.
    BleInfo::LidState m_lidState = BleInfo::LidState::UNKNOWN;
    DeviceInfo *m_deviceInfo;
    BleManager *m_bleManager;
    SystemSleepMonitor *m_systemSleepMonitor = nullptr;
    Notifier *m_notifier = nullptr;
    QString m_phoneMacStatus;
};

int main(int argc, char *argv[]) {
    // Read before the application object exists, because --headless decides which class to construct.
    bool debugMode = false;
    bool hideOnStart = false;
    bool headless = false;
    for (int i = 1; i < argc; ++i) {
        if (QString(argv[i]) == "--debug") {
            debugMode = true;
        }
        if (QString(argv[i]) == "--hide") {
            hideOnStart = true;
        }
        // --hide still builds the whole GUI so the window can be opened later; --headless never can.
        if (QString(argv[i]) == "--headless") {
            headless = true;
        }
    }

    // Set BEFORE QApplication construction so Qt's QDBusTrayIcon uses
    // "openpods" as the SNI Id property. Quickshell's Bar.qml matches on
    // this id to route left-click to PodsMenu instead of activating the
    // main window. QSettings/QStandardPaths are NOT bound to this name
    // (we use hardcoded "AirPodsTrayApp" in QSettings ctor), so flipping
    // the application name doesn't migrate user settings.
    QCoreApplication::setApplicationName("openpods");
    QCoreApplication::setOrganizationName("openpods");

    // Constructing QApplication is what pages most of Qt Gui, Widgets, Qml and Quick in; linking them still costs.
    std::unique_ptr<QCoreApplication> appOwner;
    if (headless) {
        appOwner = std::make_unique<QCoreApplication>(argc, argv);
    } else {
        // QtWidgets fallback (any file dialog, message box, native QStyle) needs
        // a dark palette so it doesn't pop a white panel on top of our dark QML.
        // This must be set before QApplication constructor to be picked up by
        // QStyle on init.
        {
            QPalette p;
            p.setColor(QPalette::Window,          QColor("#0d0d0d"));
            p.setColor(QPalette::WindowText,      QColor("#ffffff"));
            p.setColor(QPalette::Base,            QColor("#1a1a1a"));
            p.setColor(QPalette::AlternateBase,   QColor("#141414"));
            p.setColor(QPalette::Text,            QColor("#ffffff"));
            p.setColor(QPalette::Button,          QColor("#1a1a1a"));
            p.setColor(QPalette::ButtonText,      QColor("#ffffff"));
            p.setColor(QPalette::Highlight,       QColor("#b6b6b6"));
            p.setColor(QPalette::HighlightedText, QColor("#0d0d0d"));
            p.setColor(QPalette::PlaceholderText, QColor("#8d8d8d"));
            QApplication::setPalette(p);
        }
        QQuickStyle::setStyle("Basic");
        appOwner = std::make_unique<QApplication>(argc, argv);
        QGuiApplication::styleHints()->setColorScheme(Qt::ColorScheme::Dark);
    }
    QCoreApplication &app = *appOwner;

    // POSIX signal → Qt event-loop bridge. Qt6 doesn't catch SIGTERM by
    // default, so systemd-style TERM kills the process without firing
    // aboutToQuit — the control socket lingers, ~AirPodsTrayApp doesn't run,
    // reliability counters never log their final summary.
    //
    // The classic async-signal-safe pattern: signal handler writes one
    // byte to a self-pipe; a QSocketNotifier in the GUI thread reads it
    // and calls QCoreApplication::quit() from a safe context.
    static int sigPipe[2] = { -1, -1 };
    if (::pipe(sigPipe) == 0) {
        // Set close-on-exec so child processes (e.g. bluetoothctl spawned
        // via QProcess) don't inherit the pipe.
        ::fcntl(sigPipe[0], F_SETFD, FD_CLOEXEC);
        ::fcntl(sigPipe[1], F_SETFD, FD_CLOEXEC);

        struct sigaction sa{};
        sa.sa_handler = [](int){ char b = 1; ::write(sigPipe[1], &b, 1); };
        sigemptyset(&sa.sa_mask);
        sa.sa_flags = SA_RESTART;
        ::sigaction(SIGTERM, &sa, nullptr);
        ::sigaction(SIGINT,  &sa, nullptr);
        ::sigaction(SIGHUP,  &sa, nullptr);

        auto *sigNotifier = new QSocketNotifier(sigPipe[0], QSocketNotifier::Read, &app);
        QObject::connect(sigNotifier, &QSocketNotifier::activated, &app, [&app, sigNotifier]() {
            char b;
            if (::read(sigPipe[0], &b, 1) > 0) {
                LOG_INFO("Termination signal received, quitting");
            }
            sigNotifier->setEnabled(false);
            QCoreApplication::quit();
        });
    } else {
        LOG_WARN("Could not create signal pipe; SIGTERM will skip cleanup");
    }

    // Load translations. Stack-allocate so static analyzers can see the
    // lifetime; QApplication::installTranslator keeps an internal pointer
    // for the duration. translator lives until main returns, which is
    // also when app exec() returns — safe.
    QTranslator translator;
    QString locale = QLocale::system().name();

    // Try to load translation from various locations. OpenPods-prefix
    // paths are searched first so a future repackage to /usr/share/openpods
    // wins without removing the legacy librepods/ entries, which keeps
    // existing user installs working during the transition.
    QStringList translationPaths = {
        QCoreApplication::applicationDirPath() + "/translations",
        QStandardPaths::writableLocation(QStandardPaths::GenericDataLocation) + "/openpods/translations",
        "/usr/share/openpods/translations",
        "/usr/local/share/openpods/translations",
        QStandardPaths::writableLocation(QStandardPaths::GenericDataLocation) + "/librepods/translations",
        "/usr/share/librepods/translations",
        "/usr/local/share/librepods/translations"
    };

    for (const QString &path : translationPaths) {
        if (translator.load("openpods_" + locale, path)
            || translator.load("librepods_" + locale, path)) {
            app.installTranslator(&translator);
            break;
        }
    }

    // Refuse to run rather than fall back to /tmp, which is what this replaced.
    const QString ipcPath = OpenPods::Ipc::socketPath();
    if (ipcPath.isEmpty()) {
        LOG_ERROR("XDG_RUNTIME_DIR is unset; cannot place the control socket");
        return 1;
    }

    // Single-instance guard. Try to connect to an existing daemon's
    // QLocalServer first. If the connect succeeds, another instance
    // is alive — ask it to reopen its window + exit cleanly. Only
    // when the connect fails do we tear down the stale socket file
    // and bind our own server below.
    //
    // Order is load-bearing: removing the socket file before the
    // connect attempt (the previous behavior) made the guard a
    // no-op — connectToServer can't reach a unix socket whose file
    // we already deleted, so every launch spawned a fresh daemon.
    // That manifested as walker / desktop-file double-click
    // spawning new instances instead of surfacing the existing
    // window.
    {
        QLocalSocket socket_check;
        socket_check.connectToServer(ipcPath);
        if (socket_check.waitForConnected(300)) {
            LOG_INFO("Another instance already running! Reopening window...");
            socket_check.write("reopen");
            socket_check.flush();
            socket_check.waitForBytesWritten(200);
            socket_check.disconnectFromServer();
            return 0;
        }
    }

    // No existing daemon answered, so unlink the socket a crashed-or-SIGKILL'd
    // predecessor left behind before binding our own.
    QLocalServer::removeServer(ipcPath);

    if (!headless) {
        QGuiApplication::setDesktopFileName("me.kavishdevar.librepods");
        QGuiApplication::setQuitOnLastWindowClosed(false);
    }

    // Same lifetime as the stack object this replaced, so the engine still deletes trayApp on the way out.
    std::unique_ptr<QQmlApplicationEngine> engineOwner;
    if (!headless) {
        engineOwner = std::make_unique<QQmlApplicationEngine>();
        qmlRegisterType<Battery>("me.kavishdevar.Battery", 1, 0, "Battery");
        qmlRegisterType<DeviceInfo>("me.kavishdevar.DeviceInfo", 1, 0, "DeviceInfo");
    }
    QQmlApplicationEngine *engine = engineOwner.get();

    AirPodsTrayApp *trayApp = new AirPodsTrayApp(debugMode, hideOnStart, headless, engine);
    // Headless has no engine to be parented to, and the destructor logs the final reliability summary.
    if (headless) {
        trayApp->setParent(&app);
    }

    if (!headless) {
        engine->rootContext()->setContextProperty("airPodsTrayApp", trayApp);

        // Expose PHONE_MAC_ADDRESS environment variable to QML for placeholder in settings
        {
            QProcessEnvironment env = QProcessEnvironment::systemEnvironment();
            QString phoneMacEnv = env.value("PHONE_MAC_ADDRESS", "");
            engine->rootContext()->setContextProperty("PHONE_MAC_ADDRESS", phoneMacEnv);
            // Initialize the visible status in the GUI
            trayApp->updatePhoneMacStatus(phoneMacEnv.isEmpty() ? QStringLiteral("No phone MAC set") : phoneMacEnv);
        }

        engine->addImageProvider("qrcode", new QRCodeImageProvider());
    }

    // Defer Main.qml load when --hide. The daemon's tray icon + low-
    // battery toast + status IPC + AAP/BLE paths are all pure C++ —
    // no QML root needed until the user explicitly opens the window.
    // Loading Main.qml eagerly costs ~60-100 MB of QML engine
    // overhead (Qt's QML runtime + Material/Basic style + every
    // imported singleton instantiated). On a --hide autostart the
    // user may never open the window in a session, so eager load is
    // pure waste. onOpenApp / onOpenSettings / the "reopen" IPC verb
    // all check rootObjects() and lazy-load on demand.
    if (!headless && !hideOnStart) {
        trayApp->loadMainModule();
    }

    // One second is far below any rate a human notices and far above the cost of
    // serialising a small object in process, which is what this replaces polling with.
    QTimer stateWriter;
    stateWriter.setInterval(1000);
    QObject::connect(&stateWriter, &QTimer::timeout, trayApp, [trayApp]() { trayApp->writeStateFile(); });
    stateWriter.start();

    QLocalServer server;
    QLocalServer::removeServer(ipcPath);

    if (!server.listen(ipcPath))
    {
        LOG_ERROR("Unable to start the listening server");
        LOG_DEBUG("Server error: " << server.errorString());
    }
    else
    {
        LOG_DEBUG("Server started, waiting for connections...");
    }
    // Capture by pointer (not reference-to-local) so the lambdas don't
    // dangle if main()'s frame is unwound by an exception or alt exit
    // path. `engine` and `trayApp` outlive the server (they're created
    // earlier), and `serverPtr` lets us avoid `[&]` capturing the local.
    QLocalServer *serverPtr = &server;
    QQmlApplicationEngine *enginePtr = engine;
    AirPodsTrayApp *trayAppPtr = trayApp;

    QObject::connect(serverPtr, &QLocalServer::newConnection,
                     serverPtr, [serverPtr, enginePtr, trayAppPtr]() {
        QLocalSocket* clientSocket = serverPtr->nextPendingConnection();
        if (!clientSocket) return;

        // Reparent to server so it dies with us if not cleaned up sooner.
        clientSocket->setParent(serverPtr);

        QObject::connect(clientSocket, &QLocalSocket::readyRead,
                         clientSocket, [clientSocket, enginePtr, trayAppPtr]() {
            QString msg = clientSocket->readAll();
            if (msg == "reopen") {
                trayAppPtr->incReopenCallsTotal();
                // A headless daemon has no window, and the caller deserves an answer rather than silence.
                if (!enginePtr) {
                    LOG_WARN("Refusing reopen: this daemon runs headless");
                    clientSocket->write("error: this daemon runs headless and has no window\n");
                    clientSocket->flush();
                    clientSocket->disconnectFromServer();
                    return;
                }
                LOG_INFO("Reopening app window");
                const auto roots = enginePtr->rootObjects();
                if (!roots.isEmpty()) {
                    QMetaObject::invokeMethod(roots.first(), "reopen", Q_ARG(QVariant, "app"));
                } else {
                    trayAppPtr->loadMainModule();
                }
            } else if (msg == "noise:off") {
                trayAppPtr->setNoiseControlModeInt(0);
            } else if (msg == "noise:anc") {
                trayAppPtr->setNoiseControlModeInt(1);
            } else if (msg == "noise:transparency") {
                trayAppPtr->setNoiseControlModeInt(2);
            } else if (msg == "noise:adaptive") {
                trayAppPtr->setNoiseControlModeInt(3);
            } else if (msg == "noise:cycle") {
                trayAppPtr->cycleNoiseControlMode();
            } else if (msg == "ear:off") {
                // Disable auto-pause/resume entirely. macOS Settings >
                // AirPods > Automatic Ear Detection off-switch parity.
                trayAppPtr->setEarDetectionBehavior(
                    static_cast<int>(MediaController::EarDetectionBehavior::Disabled));
            } else if (msg == "ear:one") {
                trayAppPtr->setEarDetectionBehavior(
                    static_cast<int>(MediaController::EarDetectionBehavior::PauseWhenOneRemoved));
            } else if (msg == "ear:both") {
                trayAppPtr->setEarDetectionBehavior(
                    static_cast<int>(MediaController::EarDetectionBehavior::PauseWhenBothRemoved));
            } else if (msg == "forget") {
                trayAppPtr->forgetDevice();
            } else if (msg == "ca:on") {
                trayAppPtr->setConversationalAwareness(true);
            } else if (msg == "ca:off") {
                trayAppPtr->setConversationalAwareness(false);
            } else if (msg == "disconnect") {
                trayAppPtr->disconnectAirPods();
            } else if (msg == "connect") {
                trayAppPtr->connectAirPods();
            } else if (msg == "onebud:on") {
                trayAppPtr->setOneBudANCMode(true);
            } else if (msg == "onebud:off") {
                trayAppPtr->setOneBudANCMode(false);
            } else if (msg.startsWith("adaptive:")) {
                // `adaptive:N` where N is 0-100. Setter is no-op if
                // noise mode isn't Adaptive (3) — the daemon refuses
                // to push the packet when the user isn't actually in
                // Adaptive mode, so the verb is safe to fire blindly
                // from automation. Pure parser is in ipcverb.hpp +
                // unit-tested in tst_ipcverb so malformed input
                // (negative, out-of-range, non-numeric, whitespace)
                // is rejected before reaching the setter.
                if (auto level = OpenPods::Ipc::parseIntVerb(msg, QStringLiteral("adaptive:"))) {
                    trayAppPtr->setAdaptiveNoiseLevel(*level);
                }
            } else if (msg == "status") {
                // Read-only status snapshot. One-line JSON; the consumer
                // (Quickshell tile, status bar) doesn't need to keep its
                // own state machine. Schema is additive — never rename
                // or remove a key without bumping a version field.
                const QJsonObject status = trayAppPtr->statusJson();
                const QByteArray line = QJsonDocument(status).toJson(QJsonDocument::Compact) + "\n";
                clientSocket->write(line);
                clientSocket->flush();
            } else {
                LOG_ERROR("Unknown message received: " << msg);
            }
            clientSocket->disconnectFromServer();
        });
        QObject::connect(clientSocket, &QLocalSocket::errorOccurred,
                         clientSocket, [clientSocket](QLocalSocket::LocalSocketError) {
            LOG_ERROR("Local socket error: " << clientSocket->errorString());
        });
        // Free the QLocalSocket once the peer hangs up — without this
        // every reopen ping leaks one socket for the lifetime of the OpenPods daemon.
        QObject::connect(clientSocket, &QLocalSocket::disconnected,
                         clientSocket, &QLocalSocket::deleteLater);
    });

    // No error handler here: newConnection is QLocalServer's only signal, so the failed listen above is the only error it reports.

    QObject::connect(&app, &QCoreApplication::aboutToQuit,
                     serverPtr, [serverPtr, ipcPath]() {
        LOG_DEBUG("Application quitting. Cleaning up local server...");
        if (serverPtr->isListening()) {
            serverPtr->close();
        }
        QLocalServer::removeServer(ipcPath);
        // An absent state file is how a watcher learns the daemon stopped.
        QFile::remove(QStandardPaths::writableLocation(QStandardPaths::GenericStateLocation)
                      + QStringLiteral("/librepods/status.json"));
    });
    // QApplication::exec sets the accessibility root, which QCoreApplication::exec does not.
    return headless ? QCoreApplication::exec() : QApplication::exec();
}

#include "main.moc"
