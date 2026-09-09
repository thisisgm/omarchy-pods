#include "playerstatuswatcher.h"
#include <QDBusConnection>
#include <QDBusPendingReply>
#include <QVariantMap>
#include <QDBusReply>
#include <QDBusConnectionInterface>
#include <QDebug>

PlayerStatusWatcher::PlayerStatusWatcher(const QString &playerService, QObject *parent)
    : QObject(parent),
      m_playerService(playerService),
      m_iface(playerService.isEmpty() ? nullptr : new QDBusInterface(playerService, "/org/mpris/MediaPlayer2",
                                 "org.mpris.MediaPlayer2.Player", QDBusConnection::sessionBus(), this)),
      m_serviceWatcher(new QDBusServiceWatcher(playerService.isEmpty() ? QStringLiteral("org.mpris.MediaPlayer2.*") : playerService,
                                               QDBusConnection::sessionBus(),
                                               QDBusServiceWatcher::WatchForOwnerChange, this))
{
    if (!QDBusConnection::sessionBus().connect(
            playerService, "/org/mpris/MediaPlayer2", "org.freedesktop.DBus.Properties",
            "PropertiesChanged", this,
            SLOT(onPropertiesChanged(QString,QVariantMap,QStringList))))
    {
        qWarning() << "PlayerStatusWatcher: failed to subscribe to PropertiesChanged for"
                   << playerService;
    }
    connect(m_serviceWatcher, &QDBusServiceWatcher::serviceOwnerChanged,
            this, &PlayerStatusWatcher::onServiceOwnerChanged);
    updateStatus();
}

void PlayerStatusWatcher::onPropertiesChanged(const QString &interface,
                                              const QVariantMap &changed,
                                              const QStringList &)
{
    if (interface == "org.mpris.MediaPlayer2.Player" && changed.contains("PlaybackStatus")) {
        if (!m_playerService.isEmpty()) {
            emit playbackStatusChanged(changed.value("PlaybackStatus").toString());
        } else {
            updateStatus();
        }
    }
}

void PlayerStatusWatcher::updateStatus() {
    if (m_iface) {
        QVariant reply = m_iface->property("PlaybackStatus");
        if (reply.isValid()) {
            emit playbackStatusChanged(reply.toString());
            return;
        }
    }
    QString current = getCurrentPlaybackStatus(m_playerService);
    emit playbackStatusChanged(current);
}

void PlayerStatusWatcher::onServiceOwnerChanged(const QString &name, const QString &, const QString &newOwner)
{
    if (!m_playerService.isEmpty()) {
        if (name == m_playerService && newOwner.isEmpty()) {
            emit playbackStatusChanged(""); // player disappeared
        } else if (name == m_playerService && !newOwner.isEmpty()) {
            updateStatus(); // player appeared/reappeared
        }
    } else {
        if (name.startsWith("org.mpris.MediaPlayer2.")) {
            updateStatus();
        }
    }
}

QString PlayerStatusWatcher::playbackStatusOf(const QString &playerService)
{
    QDBusInterface props(playerService, "/org/mpris/MediaPlayer2",
                         "org.freedesktop.DBus.Properties", QDBusConnection::sessionBus());
    if (!props.isValid()) return QString();

    QDBusReply<QVariant> reply = props.call("Get", "org.mpris.MediaPlayer2.Player", "PlaybackStatus");
    if (!reply.isValid()) return QString();

    return reply.value().toString();
}

QString PlayerStatusWatcher::getCurrentPlaybackStatus(const QString &playerService)
{
    // If a specific player was requested, query only that one.
    if (!playerService.isEmpty()) return playbackStatusOf(playerService);

    // Otherwise pick the most-active player. Priority: Playing > Paused >
    // anything else (Stopped/empty).
    QString fallback;
    QStringList services = QDBusConnection::sessionBus().interface()->registeredServiceNames().value();
    for (const QString &service : services) {
        if (!service.startsWith("org.mpris.MediaPlayer2.")) continue;
        const QString s = playbackStatusOf(service);
        if (s == "Playing") return s;
        if (s == "Paused" && fallback.isEmpty()) fallback = s;
    }
    return fallback;
}