#ifndef BLUETOOTHMONITOR_H
#define BLUETOOTHMONITOR_H

#include <QObject>
#include <QtDBus/QtDBus>

// Forward declarations for D-Bus types
typedef QMap<QDBusObjectPath, QMap<QString, QVariantMap>> ManagedObjectList;
Q_DECLARE_METATYPE(ManagedObjectList)

class BluetoothMonitor : public QObject
{
    Q_OBJECT
public:
    explicit BluetoothMonitor(QObject *parent = nullptr);
    ~BluetoothMonitor() override;

    bool checkAlreadyConnectedDevices();
    void probeDeviceConnected(const QString &macAddress, quint64 requestId);

signals:
    void deviceConnected(const QString &macAddress, const QString &deviceName, const QString &modalias);
    void deviceDisconnected(const QString &macAddress, const QString &deviceName);
    void deviceConnectionProbeFinished(const QString &macAddress, quint64 requestId,
                                       bool connected);

private slots:
    // Receive the raw QDBusMessage so we can read message.path() reliably
    // — QDBusContext::message() is only populated on outgoing service-side
    // calls, not on incoming signal dispatch.
    void onPropertiesChanged(const QDBusMessage &message);

private:
    QDBusConnection m_dbus;
    // The watchdog repeats the sweep, so only a change of error is worth logging again.
    QString m_lastSweepError;
    void registerDBusService();
};

#endif // BLUETOOTHMONITOR_H
