#include "mediaconnector.h"
#include "playbackplayers.hpp"
#include "BluetoothMonitor.h"
#include "logger.h"
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusServiceWatcher>
#include <QDBusVariant>

namespace {
constexpr int pollIntervalMs = 1000;
constexpr int queryTimeoutMs = 1500;
// A D-Bus timeout does not cancel BlueZ's underlying connection attempt.
constexpr int connectTimeoutMs = 60000;
const QString playerPath = QStringLiteral("/org/mpris/MediaPlayer2");
const QString playerInterface = QStringLiteral("org.mpris.MediaPlayer2.Player");
}

MediaConnector::MediaConnector(QSettings *settings, QObject *parent)
    : QObject(parent), m_settings(settings)
{
    qDBusRegisterMetaType<ManagedObjectList>();
    m_address = settings->value("mediaConnect/address").toString();
    m_enabled = settings->value("mediaConnect/enabled", true).toBool();
    m_paused = settings->value("mediaConnect/paused", false).toBool();
    m_clock.start();
    m_timer.setInterval(pollIntervalMs);
    connect(&m_timer, &QTimer::timeout, this, &MediaConnector::poll);
    if (!QDBusConnection::systemBus().connect("org.bluez", QString(), "org.bluez.Device1",
                                             "Disconnected", this, SLOT(onDisconnected(QDBusMessage)))) {
        LOG_ERROR("Media connect: cannot subscribe to BlueZ disconnect reasons");
    }
    auto *bluez = new QDBusServiceWatcher("org.bluez", QDBusConnection::systemBus(),
                                         QDBusServiceWatcher::WatchForOwnerChange, this);
    connect(bluez, &QDBusServiceWatcher::serviceOwnerChanged, this, [this]() {
        // BlueZ can disappear without lowering Device1.Connected first.
        m_connected = false;
        restartMonitoring();
    });
    restartMonitoring();
}

void MediaConnector::saveSettings()
{
    m_settings->setValue("mediaConnect/enabled", m_enabled);
    m_settings->setValue("mediaConnect/address", m_address);
    m_settings->setValue("mediaConnect/paused", m_paused);
    m_settings->sync();
    if (m_settings->status() != QSettings::NoError) {
        LOG_ERROR("Media connect: cannot save preferences to " << m_settings->fileName());
    }
}

void MediaConnector::restartMonitoring()
{
    ++m_generation;
    m_policy = MediaConnectPolicy();
    if (active()) m_timer.start();
    else m_timer.stop();
    LOG_INFO("Media connect: " << (!m_enabled ? "disabled" : m_paused ? "paused after local disconnect"
                                         : m_address.isEmpty() ? "waiting for first AirPods connection" : "monitoring"));
}

void MediaConnector::setEnabled(bool enabled)
{
    if (m_enabled == enabled) return;
    m_enabled = enabled;
    saveSettings();
    restartMonitoring();
}

void MediaConnector::deviceConnected(const QString &address)
{
    m_connected = true;
    if (m_address != address || m_paused) {
        m_address = address;
        m_paused = false;
        saveSettings();
        restartMonitoring();
    }
    m_policy.satisfy();
}

void MediaConnector::onDisconnected(const QDBusMessage &message)
{
    const QString suffix = "/dev_" + QString(m_address).replace(':', '_');
    if (m_address.isEmpty() || !message.path().endsWith(suffix)) return;
    // Disconnected("org.bluez.Reason.Local", "Connection terminated by local host")
    const auto arguments = message.arguments();
    if (arguments.size() != 2 || arguments.at(0).toString() != "org.bluez.Reason.Local") return;
    m_connected = false;
    m_paused = true;
    saveSettings();
    restartMonitoring();
}

void MediaConnector::deviceDisconnected(const QString &address)
{
    if (address != m_address) return;
    m_connected = false;
    // A switch away during playback must not be followed by another takeover.
    m_policy.satisfy();
}

void MediaConnector::poll()
{
    if (m_pollBusy || !active()) return;
    m_pollBusy = true;
    const quint64 generation = m_generation;
    auto message = QDBusMessage::createMethodCall("org.freedesktop.DBus", "/org/freedesktop/DBus",
                                                 "org.freedesktop.DBus", "ListNames");
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(message, queryTimeoutMs), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, generation](QDBusPendingCallWatcher *finished) {
        QDBusPendingReply<QStringList> reply = *finished;
        finished->deleteLater();
        if (reply.isError()) {
            LOG_WARN("Media connect: ListNames failed: " << reply.error().message());
            playbackObserved(std::nullopt, generation);
            return;
        }
        QStringList players;
        for (const auto &name : reply.value()) {
            if (isAutoConnectPlayer(name)) players.append(name);
        }
        queryPlayers(players, generation);
    });
}

void MediaConnector::queryPlayers(QStringList services, quint64 generation, bool unknown)
{
    if (!active() || generation != m_generation || services.isEmpty()) {
        playbackObserved(unknown ? std::nullopt : std::optional<bool>(false), generation);
        return;
    }
    const QString service = services.takeFirst();
    // Chromium's empty introspection requires an explicit Properties.Get call.
    auto message = QDBusMessage::createMethodCall(service, playerPath, "org.freedesktop.DBus.Properties", "Get");
    message << playerInterface << QStringLiteral("PlaybackStatus");
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(message, queryTimeoutMs), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this,
            [this, services, service, generation, unknown](QDBusPendingCallWatcher *finished) {
        QDBusPendingReply<QDBusVariant> reply = *finished;
        finished->deleteLater();
        const auto playing = reply.isError() ? std::nullopt : playerIsPlaying(reply.value().variant().toString());
        if (reply.isError()) {
            LOG_DEBUG("Media connect: PlaybackStatus unavailable for " << service << ": " << reply.error().message());
        } else if (!playing.has_value()) {
            LOG_WARN("Media connect: invalid PlaybackStatus from " << service << ": " << reply.value().variant());
        }
        if (playing.value_or(false)) {
            playbackObserved(true, generation);
            return;
        }
        queryPlayers(services, generation, unknown || !playing.has_value());
    });
}

void MediaConnector::playbackObserved(std::optional<bool> playing, quint64 generation)
{
    m_pollBusy = false;
    if (!active() || generation != m_generation) return;
    if (m_policy.observe(m_clock.elapsed(), playing, m_connected, m_connectBusy)) {
        requestConnection(generation);
    }
}

bool MediaConnector::canConnect(quint64 generation) const
{
    return active() && generation == m_generation && !m_connected && m_policy.connectionStillWanted();
}

void MediaConnector::requestConnection(quint64 generation)
{
    m_connectBusy = true;
    auto message = QDBusMessage::createMethodCall("org.bluez", "/", "org.freedesktop.DBus.ObjectManager", "GetManagedObjects");
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::systemBus().asyncCall(message, queryTimeoutMs), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, generation](QDBusPendingCallWatcher *finished) {
        QDBusPendingReply<ManagedObjectList> reply = *finished;
        finished->deleteLater();
        m_connectBusy = false;
        if (!canConnect(generation)) return;
        if (reply.isError()) {
            LOG_WARN("Media connect: BlueZ device lookup failed: " << reply.error().message());
            return;
        }
        const auto objects = reply.value();
        for (auto it = objects.cbegin(); it != objects.cend(); ++it) {
            const QVariantMap device = it.value().value("org.bluez.Device1");
            if (device.value("Address").toString() != m_address) continue;
            if (device.value("Connected").toBool()) {
                m_connected = true;
                m_policy.satisfy();
                return;
            }
            if (!device.value("Paired").toBool() || !device.value("Trusted").toBool()) {
                LOG_WARN("Media connect: remembered AirPods must be paired and trusted");
                m_policy.satisfy();
                return;
            }
            auto request = QDBusMessage::createMethodCall("org.bluez", it.key().path(), "org.bluez.Device1", "Connect");
            m_connectBusy = true;
            LOG_INFO("Media connect: requesting AirPods connection for media playback");
            auto *connection = new QDBusPendingCallWatcher(QDBusConnection::systemBus().asyncCall(request, connectTimeoutMs), this);
            connect(connection, &QDBusPendingCallWatcher::finished, this, [this, generation](QDBusPendingCallWatcher *done) {
                QDBusPendingReply<> result = *done;
                done->deleteLater();
                m_connectBusy = false;
                if (generation == m_generation) m_policy.connectionFinished(m_clock.elapsed());
                if (result.isError()) {
                    LOG_WARN("Media connect: BlueZ Connect failed: " << result.error().name() << ": " << result.error().message());
                } else if (generation == m_generation) {
                    m_policy.satisfy();
                    LOG_INFO("Media connect: BlueZ Connect succeeded");
                }
            });
            return;
        }
        LOG_WARN("Media connect: remembered AirPods not found in BlueZ; connect them manually once");
        m_policy.satisfy();
    });
}
