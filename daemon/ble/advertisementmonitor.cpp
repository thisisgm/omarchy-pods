#include "advertisementmonitor.h"
#include "logger.h"

#include <QDBusArgument>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusMetaType>
#include <QDBusObjectPath>
#include <QDBusPendingCallWatcher>
#include <QDBusReply>
#include <QDBusVariant>

namespace {
const QString bluezService = QStringLiteral("org.bluez");
const QString adapterPath = QStringLiteral("/org/bluez/hci0");
const QString managerInterface = QStringLiteral("org.bluez.AdvertisementMonitorManager1");
const QString monitorInterface = QStringLiteral("org.bluez.AdvertisementMonitor1");
const QString deviceInterface = QStringLiteral("org.bluez.Device1");
const QString propertiesInterface = QStringLiteral("org.freedesktop.DBus.Properties");
const QString objectManagerInterface = QStringLiteral("org.freedesktop.DBus.ObjectManager");
const QString appPath = QStringLiteral("/org/librepods/advertisementmonitor");
const QString monitorPath = appPath + QStringLiteral("/apple");
constexpr quint16 appleCompanyId = 0x004C;
constexpr uchar manufacturerDataAdType = 0xFF;

// One or_patterns entry, D-Bus (yyay): start offset in the AD payload, AD type, bytes to match.
struct Pattern
{
    uchar start = 0;
    uchar adType = 0;
    QByteArray content;
};
} // namespace

Q_DECLARE_METATYPE(Pattern)
using PatternList = QList<Pattern>;
using InterfaceMap = QMap<QString, QVariantMap>;
using ManagedObjects = QMap<QDBusObjectPath, InterfaceMap>;

QDBusArgument &operator<<(QDBusArgument &argument, const Pattern &pattern)
{
    argument.beginStructure();
    argument << pattern.start << pattern.adType << pattern.content;
    argument.endStructure();
    return argument;
}

const QDBusArgument &operator>>(const QDBusArgument &argument, Pattern &pattern)
{
    argument.beginStructure();
    argument >> pattern.start >> pattern.adType >> pattern.content;
    argument.endStructure();
    return argument;
}

AdvertisementMonitor::AdvertisementMonitor(QObject *parent) : QDBusVirtualObject(parent)
{
    qDBusRegisterMetaType<Pattern>();
    qDBusRegisterMetaType<PatternList>();
    qDBusRegisterMetaType<InterfaceMap>();
    qDBusRegisterMetaType<ManagedObjects>();
}

bool AdvertisementMonitor::start()
{
    if (m_active)
        return true;

    QDBusConnection bus = QDBusConnection::systemBus();
    QDBusMessage introspection = QDBusMessage::createMethodCall(
        bluezService, adapterPath, QStringLiteral("org.freedesktop.DBus.Introspectable"), QStringLiteral("Introspect"));
    QDBusReply<QString> xml = bus.call(introspection);
    if (!xml.isValid() || !xml.value().contains(managerInterface)) {
        LOG_INFO("BlueZ advertisement monitor unavailable on" << adapterPath << ", using a full BLE scan");
        return false;
    }

    if (!bus.registerVirtualObject(appPath, this, QDBusConnection::SubPath)) {
        LOG_ERROR("Advertisement monitor: cannot export" << appPath << ":" << bus.lastError().message());
        return false;
    }

    QDBusMessage call = QDBusMessage::createMethodCall(bluezService, adapterPath, managerInterface, QStringLiteral("RegisterMonitor"));
    call << QVariant::fromValue(QDBusObjectPath(appPath));
    auto *watcher = new QDBusPendingCallWatcher(bus.asyncCall(call), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *finished) {
        finished->deleteLater();
        if (!finished->isError()) {
            LOG_INFO("BlueZ advertisement monitor registered for Apple proximity adverts");
            return;
        }
        LOG_ERROR("Advertisement monitor: RegisterMonitor on" << adapterPath << "failed:" << finished->error().message());
        // A stopScan that ran while this call was pending must not be undone by the fallback.
        if (!m_active)
            return;
        stop();
        emit registrationFailed();
    });

    m_active = true;
    return true;
}

void AdvertisementMonitor::stop()
{
    if (!m_active)
        return;
    m_active = false;

    QDBusConnection bus = QDBusConnection::systemBus();
    QDBusMessage call = QDBusMessage::createMethodCall(bluezService, adapterPath, managerInterface, QStringLiteral("UnregisterMonitor"));
    call << QVariant::fromValue(QDBusObjectPath(appPath));
    bus.asyncCall(call);
    bus.unregisterObject(appPath, QDBusConnection::UnregisterTree);

    const QStringList watched = m_devices.keys();
    for (const QString &devicePath : watched)
        unwatchDevice(devicePath);
}

QString AdvertisementMonitor::introspect(const QString &path) const
{
    if (path == monitorPath)
        return QStringLiteral("<interface name=\"org.bluez.AdvertisementMonitor1\">"
                              "<method name=\"Release\"/><method name=\"Activate\"/>"
                              "<method name=\"DeviceFound\"><arg name=\"device\" type=\"o\" direction=\"in\"/></method>"
                              "<method name=\"DeviceLost\"><arg name=\"device\" type=\"o\" direction=\"in\"/></method>"
                              "</interface>");
    if (path == appPath)
        return QStringLiteral("<interface name=\"org.freedesktop.DBus.ObjectManager\">"
                              "<method name=\"GetManagedObjects\"><arg type=\"a{oa{sa{sv}}}\" direction=\"out\"/></method>"
                              "</interface><node name=\"apple\"/>");
    return QString();
}

bool AdvertisementMonitor::handleMessage(const QDBusMessage &message, const QDBusConnection &connection)
{
    if (message.path() == appPath && message.interface() == objectManagerInterface
        && message.member() == QLatin1String("GetManagedObjects")) {
        // Sample match: AD type 0xFF payload 4C 00 07 ..., Apple's company id then the proximity-pairing type.
        const PatternList patterns{{0, manufacturerDataAdType, QByteArray::fromHex("4c0007")}};
        QVariantMap properties;
        properties.insert(QStringLiteral("Type"), QStringLiteral("or_patterns"));
        properties.insert(QStringLiteral("Patterns"), QVariant::fromValue(patterns));
        ManagedObjects objects;
        objects.insert(QDBusObjectPath(monitorPath), InterfaceMap{{monitorInterface, properties}});
        connection.send(message.createReply(QVariant::fromValue(objects)));
        return true;
    }

    if (message.path() != monitorPath || message.interface() != monitorInterface)
        return false;

    const QString member = message.member();
    if (member == QLatin1String("DeviceFound") || member == QLatin1String("DeviceLost")) {
        const QString devicePath = message.arguments().value(0).value<QDBusObjectPath>().path();
        if (member == QLatin1String("DeviceFound"))
            watchDevice(devicePath);
        else
            unwatchDevice(devicePath);
    } else if (member == QLatin1String("Release")) {
        LOG_WARN("BlueZ released the advertisement monitor");
        QMetaObject::invokeMethod(this, &AdvertisementMonitor::stop, Qt::QueuedConnection);
    } else if (member != QLatin1String("Activate")) {
        return false;
    }
    connection.send(message.createReply());
    return true;
}

void AdvertisementMonitor::watchDevice(const QString &devicePath)
{
    if (m_devices.contains(devicePath))
        return;

    QDBusConnection bus = QDBusConnection::systemBus();
    QDBusMessage getAll = QDBusMessage::createMethodCall(bluezService, devicePath, propertiesInterface, QStringLiteral("GetAll"));
    getAll << deviceInterface;
    QDBusReply<QVariantMap> reply = bus.call(getAll);
    if (!reply.isValid()) {
        LOG_WARN("Advertisement monitor: cannot read" << devicePath << ":" << reply.error().message());
        return;
    }

    m_devices.insert(devicePath, QBluetoothAddress(reply.value().value(QStringLiteral("Address")).toString()));
    // BlueZ fires PropertiesChanged for each new advert payload, which is how battery updates keep arriving.
    bus.connect(bluezService, devicePath, propertiesInterface, QStringLiteral("PropertiesChanged"),
                this, SLOT(onPropertiesChanged(QDBusMessage)));
    emitDevice(devicePath, reply.value().value(QStringLiteral("ManufacturerData")));
}

void AdvertisementMonitor::unwatchDevice(const QString &devicePath)
{
    if (!m_devices.remove(devicePath))
        return;
    QDBusConnection::systemBus().disconnect(bluezService, devicePath, propertiesInterface, QStringLiteral("PropertiesChanged"),
                                            this, SLOT(onPropertiesChanged(QDBusMessage)));
}

void AdvertisementMonitor::onPropertiesChanged(const QDBusMessage &message)
{
    const QList<QVariant> arguments = message.arguments();
    if (arguments.value(0).toString() != deviceInterface)
        return;
    const QVariantMap changed = qdbus_cast<QVariantMap>(arguments.value(1));
    if (changed.contains(QStringLiteral("ManufacturerData")))
        emitDevice(message.path(), changed.value(QStringLiteral("ManufacturerData")));
}

// Sample input: a{qv} {0x004C: <ay 07 19 01 27 20 ...>}, the BlueZ Device1 ManufacturerData property.
void AdvertisementMonitor::emitDevice(const QString &devicePath, const QVariant &manufacturerData)
{
    if (!manufacturerData.canConvert<QDBusArgument>())
        return;
    const QDBusArgument argument = manufacturerData.value<QDBusArgument>();
    QBluetoothDeviceInfo info(m_devices.value(devicePath), QString(), 0);
    argument.beginMap();
    while (!argument.atEnd()) {
        quint16 companyId = 0;
        QDBusVariant value;
        argument.beginMapEntry();
        argument >> companyId >> value;
        argument.endMapEntry();
        if (companyId == appleCompanyId)
            info.setManufacturerData(companyId, value.variant().toByteArray());
    }
    argument.endMap();

    if (info.manufacturerData().contains(appleCompanyId))
        emit deviceSeen(info);
}
