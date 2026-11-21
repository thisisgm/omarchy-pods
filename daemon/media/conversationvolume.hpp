#pragma once

#include <QDebug>
#include <QProcess>
#include <QTimer>
#include <QVariantAnimation>

class ConversationVolume : public QObject
{
public:
    explicit ConversationVolume(QObject *parent = nullptr) : QObject(parent)
    {
        m_timeout.setSingleShot(true);
        connect(&m_timeout, &QTimer::timeout, this, [this] { m_command.kill(); });
        connect(&m_command, &QProcess::finished, this, [this] { finishWrite(); });
        connect(&m_command, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
            if (error == QProcess::FailedToStart)
                finishWrite();
        });
        m_fade.setEasingCurve(QEasingCurve::InOutSine);
        connect(&m_fade, &QVariantAnimation::valueChanged, this, [this](const QVariant &value) {
            if (m_fade.state() != QAbstractAnimation::Running)
                return;
            m_pendingGain = value.toDouble();
            m_pendingWrite = true;
            advance();
        });
    }

    ~ConversationVolume() override
    {
        m_fade.stop();
        m_timeout.stop();
        m_command.disconnect(this);
        // The event loop is leaving, so shutdown alone waits for the last write before restoring.
        if (m_inFlight && !m_command.waitForFinished(commandTimeoutMs)) {
            m_command.kill();
            m_command.waitForFinished(commandTimeoutMs);
        }
        if (!m_sink.isEmpty() && m_dirty) {
            for (int attempt = 0; attempt < restoreAttempts; ++attempt) {
                m_command.start(QStringLiteral("pw-cli"), arguments(1.0));
                const bool finished = m_command.waitForFinished(commandTimeoutMs);
                const QByteArray error = m_command.readAllStandardError();
                if (finished && succeeded(error))
                    break;
                qWarning() << "PipeWire conversation shutdown restore failed for" << m_sink << error;
                if (!finished) {
                    m_command.kill();
                    m_command.waitForFinished(commandTimeoutMs);
                }
            }
        }
    }

    void setSpeaking(bool speaking, const QString &sink)
    {
        if (sink != m_requestedSink || speaking != m_speaking) {
            m_fade.stop();
            m_pendingWrite = false;
        }
        m_requestedSink = sink;
        m_speaking = speaking;
        advance();
    }

    void reset()
    {
        m_fade.stop();
        m_pendingWrite = false;
        m_requestedSink.clear();
        m_speaking = false;
        advance();
    }

private:
    QStringList arguments(double gain) const
    {
        // pw-cli resolves node.name; software gain leaves Bluetooth's coarse hardware volume alone.
        return {QStringLiteral("set-param"), m_sink, QStringLiteral("Props"),
                QStringLiteral("{ volume: %1 }").arg(gain, 0, 'f', 6)};
    }

    bool succeeded(const QByteArray &error) const
    {
        return m_command.exitStatus() == QProcess::NormalExit && m_command.exitCode() == 0
            && !error.contains("Error:");
    }

    void write(double gain, bool restoring)
    {
        m_inFlight = true;
        m_restoring = restoring;
        m_commandGain = gain;
        m_dirty = m_dirty || gain != 1.0;
        m_timeout.start(commandTimeoutMs);
        m_command.start(QStringLiteral("pw-cli"), arguments(gain));
    }

    void finishWrite()
    {
        if (!m_inFlight)
            return;
        m_inFlight = false;
        m_timeout.stop();
        const QByteArray error = m_command.readAllStandardError();
        m_command.readAllStandardOutput();
        const bool ok = m_command.error() != QProcess::FailedToStart && succeeded(error);
        // A destroyed sink has no gain left to restore and must not block a replacement sink.
        const bool disappeared = error.contains("unknown global");
        if (ok || (m_restoring && disappeared)) {
            m_gain = m_commandGain;
            m_dirty = m_gain != 1.0;
            if (!m_dirty)
                m_restoreFailures = 0;
            if (m_restoring)
                m_sink.clear();
        } else {
            qWarning() << "PipeWire conversation volume failed for" << m_sink
                       << m_command.errorString() << error;
            m_fade.stop();
            m_pendingWrite = false;
            if (m_restoring) {
                if (++m_restoreFailures >= restoreAttempts)
                    return;
            } else if (m_requestedSink == m_sink) {
                m_requestedSink.clear();
            }
        }
        advance();
    }

    void advance()
    {
        if (m_inFlight || m_restoreFailures >= restoreAttempts)
            return;
        if (m_sink != m_requestedSink) {
            if (!m_sink.isEmpty() && m_dirty) {
                write(1.0, true);
                return;
            }
            m_sink = m_requestedSink;
            m_gain = 1.0;
        }
        if (m_sink.isEmpty())
            return;
        if (m_pendingWrite) {
            m_pendingWrite = false;
            write(m_pendingGain, false);
            return;
        }
        if (m_fade.state() == QAbstractAnimation::Running)
            return;
        // PulseAudio's 20% volume is cubic, so the corresponding software gain is 0.2 cubed.
        constexpr double conversationGain = 0.008;
        const double target = m_speaking ? conversationGain : 1.0;
        if (m_gain == target)
            return;
        m_fade.setStartValue(m_gain);
        m_fade.setEndValue(target);
        constexpr int fadeDownMs = 600;
        constexpr int fadeUpMs = 1000;
        m_fade.setDuration(m_speaking ? fadeDownMs : fadeUpMs);
        m_fade.start();
    }

    static constexpr int commandTimeoutMs = 2000;
    static constexpr int restoreAttempts = 2;
    QVariantAnimation m_fade;
    QProcess m_command;
    QTimer m_timeout;
    QString m_sink;
    QString m_requestedSink;
    double m_gain = 1.0;
    double m_pendingGain = 1.0;
    double m_commandGain = 1.0;
    bool m_speaking = false;
    bool m_pendingWrite = false;
    bool m_inFlight = false;
    bool m_restoring = false;
    bool m_dirty = false;
    int m_restoreFailures = 0;
};
