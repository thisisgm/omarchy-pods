#pragma once

#include <QObject>
#include <QDBusInterface>
#include <QDBusServiceWatcher>

class PlayerStatusWatcher : public QObject {
    Q_OBJECT
public:
    explicit PlayerStatusWatcher(const QString &playerService, QObject *parent = nullptr);
    static QString getCurrentPlaybackStatus(const QString &playerService);
    // Chromium serves an empty Introspect document, so the metaobject QDBusInterface builds
    // from it has no PlaybackStatus and property() reads invalid on a player that is Playing.
    static QString playbackStatusOf(const QString &playerService);

signals:
    void playbackStatusChanged(const QString &status);

private slots:
    void onPropertiesChanged(const QString &interface, const QVariantMap &changed, const QStringList &);
    void onServiceOwnerChanged(const QString &name, const QString &oldOwner, const QString &newOwner);

private:
    void updateStatus();
    QString m_playerService;
    // Same defensive default-init as blemanager.h (iter-57): the ctor
    // body assigns these via parented `new`, but a partial-construction
    // failure or a future ctor-overload that skips the body would leave
    // updateStatus() dereferencing a dangling pointer.
    QDBusInterface *m_iface = nullptr;
    QDBusServiceWatcher *m_serviceWatcher = nullptr;
};