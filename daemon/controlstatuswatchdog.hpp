#pragma once

#include <QObject>
#include <QTimer>

#include "controlreconnect.hpp"
#include "logger.h"

// Waits for a new control link to report battery, and says what to try when it does not.
class ControlStatusWatchdog : public QObject
{
    Q_OBJECT

public:
    enum class LinkStatus { Waiting, EarOnly, BatterySeen };

    explicit ControlStatusWatchdog(int waitMs = ControlReconnect::statusWaitMs, QObject *parent = nullptr)
        : QObject(parent)
    {
        m_timer.setSingleShot(true);
        m_timer.setInterval(waitMs);
        connect(&m_timer, &QTimer::timeout, this, &ControlStatusWatchdog::onTimeout);
    }

    LinkStatus status() const { return m_status; }

    // A new socket starts its own handshake and request counts, but the reopen count spans the BlueZ connection.
    void linkOpened()
    {
        m_status = LinkStatus::Waiting;
        m_handshakesResent = 0;
        m_batteryRequestsSent = 0;
        m_timer.start();
    }

    void linkClosed() { m_timer.stop(); }

    void bluezConnected() { m_socketsReopened = 0; }

    void earStatusReceived()
    {
        if (m_status == LinkStatus::Waiting) {
            m_status = LinkStatus::EarOnly;
        }
        m_socketsReopened = 0;
    }

    void batteryReceived()
    {
        m_status = LinkStatus::BatterySeen;
        m_socketsReopened = 0;
        m_timer.stop();
    }

signals:
    void resendHandshake();
    void requestNotifications();
    void reopenSocket();

private:
    void onTimeout()
    {
        switch (m_status) {
        case LinkStatus::BatterySeen:
            return;
        case LinkStatus::EarOnly:
            if (!ControlReconnect::hasAttemptRemaining(m_batteryRequestsSent, ControlReconnect::batteryRequestLimit)) {
                LOG_WARN("AirPods sent no battery after " << m_batteryRequestsSent
                         << " repeated notification requests, it arrives with the next level change");
                return;
            }
            ++m_batteryRequestsSent;
            LOG_INFO("AirPods sent ear status but no battery, requesting notifications again ("
                     << m_batteryRequestsSent << "/" << ControlReconnect::batteryRequestLimit << ")");
            m_timer.start();
            emit requestNotifications();
            return;
        case LinkStatus::Waiting:
            break;
        }

        switch (ControlReconnect::nextSilentLinkStep(m_handshakesResent, m_socketsReopened)) {
        case ControlReconnect::SilentLinkStep::ResendHandshake:
            ++m_handshakesResent;
            LOG_WARN("AirPods sent no battery or ear status " << m_timer.interval()
                     << "ms after the handshake, re-sending it ("
                     << m_handshakesResent << "/" << ControlReconnect::statusHandshakeResendLimit << ")");
            m_timer.start();
            emit resendHandshake();
            return;
        case ControlReconnect::SilentLinkStep::ReopenSocket:
            ++m_socketsReopened;
            LOG_WARN("AirPods sent no battery or ear status after " << (m_handshakesResent + 1)
                     << " handshakes, opening a new control link ("
                     << m_socketsReopened << "/" << ControlReconnect::silentLinkReopenLimit << ")");
            emit reopenSocket();
            return;
        case ControlReconnect::SilentLinkStep::GiveUp:
            LOG_ERROR("AirPods sent no battery or ear status on " << (m_socketsReopened + 1)
                      << " control links, restart librepods to recover");
            return;
        }
    }

    QTimer m_timer;
    LinkStatus m_status = LinkStatus::Waiting;
    int m_handshakesResent = 0;
    int m_batteryRequestsSent = 0;
    int m_socketsReopened = 0;
};
