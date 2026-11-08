#ifndef ADVERTISEMENTMONITOR_H
#define ADVERTISEMENTMONITOR_H

#include <QBluetoothAddress>
#include <QBluetoothDeviceInfo>
#include <QDBusVirtualObject>
#include <QHash>

class QDBusMessage;

// Asks BlueZ for Apple proximity-pairing adverts only, so the other devices in range never reach the session bus.
class AdvertisementMonitor : public QDBusVirtualObject
{
    Q_OBJECT
public:
    explicit AdvertisementMonitor(QObject *parent = nullptr);

    // False when bluetoothd runs without Experimental = true, which is what hides the monitor manager.
    bool start();
    void stop();
    bool isActive() const { return m_active; }

    QString introspect(const QString &path) const override;
    bool handleMessage(const QDBusMessage &message, const QDBusConnection &connection) override;

signals:
    void deviceSeen(const QBluetoothDeviceInfo &info);
    void registrationFailed();

private slots:
    void onPropertiesChanged(const QDBusMessage &message);

private:
    void watchDevice(const QString &devicePath);
    void unwatchDevice(const QString &devicePath);
    void emitDevice(const QString &devicePath, const QVariant &manufacturerData);

    bool m_active = false;
    QHash<QString, QBluetoothAddress> m_devices;
};

#endif // ADVERTISEMENTMONITOR_H
