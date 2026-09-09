#pragma once

#include <QObject>
#include <QProcess>
#include <QStandardPaths>
#include <QString>

// Split out of TrayIconManager so a headless run still gets user-visible toasts.
class Notifier : public QObject
{
    Q_OBJECT

public:
    explicit Notifier(QObject *parent = nullptr) : QObject(parent) {}

    bool enabled() const { return m_enabled; }

    void setEnabled(bool enabled)
    {
        if (m_enabled != enabled)
        {
            m_enabled = enabled;
            emit enabledChanged(enabled);
        }
    }

    // Without omarchy on PATH the tray takes over through fallbackRequested, and headless has no listener.
    void notify(const QString &title, const QString &message, const QString &appName = QString())
    {
        if (!m_enabled) {
            return;
        }
        const QString omarchy = QStandardPaths::findExecutable(QStringLiteral("omarchy"));
        if (omarchy.isEmpty()) {
            emit fallbackRequested(title, message);
            return;
        }
        const QString resolvedAppName = appName.isEmpty() ? QStringLiteral("AirPods") : appName;
        QProcess::startDetached(omarchy, {QStringLiteral("notification"), QStringLiteral("send"),
                                          QStringLiteral("--app-name"), resolvedAppName,
                                          QStringLiteral("-g"), QStringLiteral("\uF025"),
                                          title, message});
    }

signals:
    void enabledChanged(bool enabled);
    void fallbackRequested(const QString &title, const QString &message);

private:
    bool m_enabled = true;
};
