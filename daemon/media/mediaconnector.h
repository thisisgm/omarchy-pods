#pragma once

#include "mediaconnectpolicy.hpp"
#include <QElapsedTimer>
#include <QDBusMessage>
#include <QObject>
#include <QSettings>
#include <QStringList>
#include <QTimer>

class MediaConnector : public QObject
{
    Q_OBJECT
public:
    explicit MediaConnector(QSettings *settings, QObject *parent = nullptr);
    bool enabled() const { return m_enabled; }
    bool paused() const { return m_paused; }
    void setEnabled(bool enabled);
    void deviceConnected(const QString &address);
    void deviceDisconnected(const QString &address);

private slots:
    void onDisconnected(const QDBusMessage &message);

private:
    void restartMonitoring();
    void saveSettings();
    bool active() const { return m_enabled && !m_paused && !m_address.isEmpty(); }
    void poll();
    void queryPlayers(QStringList services, quint64 generation, bool unknown = false);
    void playbackObserved(std::optional<bool> playing, quint64 generation);
    void requestConnection(quint64 generation);
    bool canConnect(quint64 generation) const;

    QSettings *m_settings;
    QTimer m_timer;
    QElapsedTimer m_clock;
    MediaConnectPolicy m_policy;
    QString m_address;
    quint64 m_generation = 0;
    bool m_enabled = false;
    bool m_paused = false;
    bool m_connected = false;
    bool m_pollBusy = false;
    bool m_connectBusy = false;
};
