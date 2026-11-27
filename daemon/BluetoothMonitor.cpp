#include "BluetoothMonitor.h"
#include "logger.h"

#include <QDebug>
#include <QDBusObjectPath>
#include <QDBusMetaType>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>

// bluetoothd answers this sweep in milliseconds, and the watchdog repeats it, so a wedged
// bluetoothd must not hold the event loop for the 25 s D-Bus default on every tick.
static constexpr int sweepTimeoutMs = 2000;

BluetoothMonitor::BluetoothMonitor(QObject *parent)
    : QObject(parent), m_dbus(QDBusConnection::systemBus())
{
    qDBusRegisterMetaType<QDBusObjectPath>();
    qDBusRegisterMetaType<ManagedObjectList>();

    if (!m_dbus.isConnected())
    {
        LOG_WARN("Failed to connect to system D-Bus");
        return;
    }

    registerDBusService();
    checkAlreadyConnectedDevices();
}

BluetoothMonitor::~BluetoothMonitor()
{
    // The system bus is process-wide and shared with QtBluetooth/QML; do
    // NOT disconnectFromBus(name()) — that would tear down everyone else's
    // connection. Qt will clean up our signal subscription when this
    // QObject dies.
}

void BluetoothMonitor::registerDBusService()
{
    // Subscribe to PropertiesChanged but route the *whole* QDBusMessage to
    // the slot so we get the originating object path. Without QDBusMessage
    // in the slot signature, QDBusContext::message() would be unset for
    // incoming signals.
    if (!m_dbus.connect(
            QString(),                                 // service: any (signals don't carry a unique name we know up-front)
            QString(),                                 // path: any
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            this,
            SLOT(onPropertiesChanged(QDBusMessage))))
    {
        LOG_WARN("Failed to connect to D-Bus PropertiesChanged signal");
    }
}

bool BluetoothMonitor::checkAlreadyConnectedDevices()
{
    // QDBusInterface introspects the remote object from its constructor on the default 25 s
    // timeout, so setTimeout on the interface cannot bound this. Build the call directly.
    QDBusMessage request = QDBusMessage::createMethodCall(
        "org.bluez", "/", "org.freedesktop.DBus.ObjectManager", "GetManagedObjects");
    QDBusMessage reply = m_dbus.call(request, QDBus::Block, sweepTimeoutMs);

    if (reply.type() == QDBusMessage::ErrorMessage)
    {
        // Keyed on the name as well, since an error reply carrying no string reads empty.
        const QString sweepError = reply.errorName() + QStringLiteral(": ") + reply.errorMessage();
        if (sweepError != m_lastSweepError) {
            LOG_WARN("Failed to get managed objects: " << sweepError);
            m_lastSweepError = sweepError;
        }
        return false;
    }
    m_lastSweepError.clear();

    QVariant firstArg = reply.arguments().constFirst();
    QDBusArgument arg = firstArg.value<QDBusArgument>();
    ManagedObjectList managedObjects;
    arg >> managedObjects;

    bool deviceFound = false;

    for (auto it = managedObjects.constBegin(); it != managedObjects.constEnd(); ++it)
    {
        const QMap<QString, QVariantMap> &interfaces = it.value();
        if (!interfaces.contains("org.bluez.Device1")) continue;

        const QVariantMap &deviceProps = interfaces.value("org.bluez.Device1");
        if (!deviceProps.contains("UUIDs") || !deviceProps.contains("Connected") ||
            !deviceProps.contains("Address") || !deviceProps.contains("Name"))
        {
            continue;
        }

        const QStringList uuids = deviceProps["UUIDs"].toStringList();
        if (!uuids.contains("74ec2172-0bad-4d01-8f77-997b2be0722a")) continue;

        if (deviceProps["Connected"].toBool()) {
            QString macAddress = deviceProps["Address"].toString();
            QString deviceName = deviceProps["Name"].toString();
            emit deviceConnected(macAddress, deviceName, deviceProps.value("Modalias").toString());
            LOG_DEBUG("Found already connected AirPods: " << macAddress << " Name: " << deviceName);
            deviceFound = true;
        }
    }
    return deviceFound;
}

void BluetoothMonitor::probeDeviceConnected(const QString &macAddress, quint64 requestId)
{
    if (macAddress.isEmpty() || !m_dbus.isConnected()) {
        emit deviceConnectionProbeFinished(macAddress, requestId, false);
        return;
    }

    QDBusInterface objectManager("org.bluez", "/", "org.freedesktop.DBus.ObjectManager", m_dbus);
    auto *watcher = new QDBusPendingCallWatcher(objectManager.asyncCall("GetManagedObjects"), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this,
            [this, watcher, macAddress, requestId](QDBusPendingCallWatcher *) {
                bool connected = false;
                const QDBusMessage reply = watcher->reply();
                if (reply.type() == QDBusMessage::ErrorMessage || reply.arguments().isEmpty()) {
                    LOG_WARN("Failed to query BlueZ connection state for " << macAddress
                             << ": " << reply.errorMessage());
                } else {
                    const QDBusArgument arg = reply.arguments().constFirst().value<QDBusArgument>();
                    ManagedObjectList managedObjects;
                    arg >> managedObjects;

                    for (auto it = managedObjects.constBegin(); it != managedObjects.constEnd(); ++it) {
                        const QVariantMap deviceProps = it.value().value("org.bluez.Device1");
                        if (deviceProps.value("Address").toString().compare(
                                macAddress, Qt::CaseInsensitive) == 0) {
                            connected = deviceProps.value("Connected").toBool();
                            break;
                        }
                    }
                }

                emit deviceConnectionProbeFinished(macAddress, requestId, connected);
                watcher->deleteLater();
            });
}

void BluetoothMonitor::onPropertiesChanged(const QDBusMessage &message)
{
    // Filter at the path level: only BlueZ Device1 objects, never the
    // chatter from systemd/UPower/networkmanager that also lives on the
    // system bus.
    const QString path = message.path();
    if (!path.startsWith("/org/bluez/")) {
        return;
    }

    const QList<QVariant> args = message.arguments();
    if (args.size() < 2) {
        return;
    }
    const QString interface = args.at(0).toString();
    if (interface != "org.bluez.Device1") {
        return;
    }

    // PropertiesChanged signature: (string interface, dict changed, array invalidated)
    const QVariantMap changedProps = qdbus_cast<QVariantMap>(args.at(1));
    if (!changedProps.contains("Connected")) {
        return;
    }

    const bool connected = changedProps["Connected"].toBool();

    // One asynchronous GetAll, so a BlueZ that stops answering cannot hold the event loop. Replies come in call order.
    QDBusMessage request = QDBusMessage::createMethodCall("org.bluez", path, "org.freedesktop.DBus.Properties", "GetAll");
    request << QStringLiteral("org.bluez.Device1");
    auto *watcher = new QDBusPendingCallWatcher(m_dbus.asyncCall(request), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, connected](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        const QDBusPendingReply<QVariantMap> reply = *call;
        if (reply.isError()) {
            LOG_WARN("Reading Device1 properties for a connection change failed: " << reply.error().message());
            return;
        }
        const QVariantMap props = reply.value();
        if (!props.value("UUIDs").toStringList().contains("74ec2172-0bad-4d01-8f77-997b2be0722a")) {
            return;
        }
        const QString macAddress = props.value("Address").toString();
        const QString deviceName = props.value("Name", QStringLiteral("Unknown")).toString();
        if (connected) {
            emit deviceConnected(macAddress, deviceName, props.value("Modalias").toString());
            LOG_DEBUG("AirPods device connected");
        } else {
            emit deviceDisconnected(macAddress, deviceName);
            LOG_DEBUG("AirPods device disconnected");
        }
    });
}
