#pragma once

#include <QBluetoothAddress>
#include <QByteArray>
#include <QObject>
#include <QSocketNotifier>
#include <QString>

#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <sys/socket.h>
#include <unistd.h>

// The AACP control channel on a kernel L2CAP socket. Qt's socket on BlueZ 5.46 and later is a
// D-Bus profile socket opened at BlueZ's default 672-byte receive MTU, and the AirPods leave out
// any sensor service whose descriptor would not fit (the heart rate one is 921 bytes).
class ControlSocket : public QObject
{
    Q_OBJECT

public:
    static constexpr quint16 AACP_PSM = 0x1001;
    static constexpr quint16 RECEIVE_MTU = 2048;

    explicit ControlSocket(QObject *parent = nullptr) : QObject(parent) {}
    ~ControlSocket() override { close(); }

    void connectToDevice(const QBluetoothAddress &address)
    {
        close();
        m_peer = address;
        m_fd = ::socket(AF_BLUETOOTH, SOCK_SEQPACKET | SOCK_NONBLOCK | SOCK_CLOEXEC, BTPROTO_L2CAP);
        if (m_fd < 0) {
            fail(errno);
            return;
        }
        Options options {};
        socklen_t size = sizeof(options);
        if (getsockopt(m_fd, SOL_L2CAP, L2CAP_OPTIONS, &options, &size) == 0) {
            options.imtu = RECEIVE_MTU;
            setsockopt(m_fd, SOL_L2CAP, L2CAP_OPTIONS, &options, size);
        }

        Address target {};
        target.family = AF_BLUETOOTH;
        target.psm = AACP_PSM;  // little endian on the wire, which every host this runs on is
        const quint64 raw = address.toUInt64();
        for (int i = 0; i < 6; ++i)
            target.bdaddr[i] = static_cast<quint8>(raw >> (8 * i));
        if (::connect(m_fd, reinterpret_cast<sockaddr *>(&target), sizeof(target)) != 0 && errno != EINPROGRESS) {
            fail(errno);
            return;
        }
        m_writeNotifier = new QSocketNotifier(m_fd, QSocketNotifier::Write, this);
        connect(m_writeNotifier, &QSocketNotifier::activated, this, &ControlSocket::finishConnect);
    }

    bool isConnected() const { return m_connected; }
    QBluetoothAddress peerAddress() const { return m_peer; }
    QString errorString() const { return m_error; }

    bool write(const QByteArray &packet)
    {
        return m_connected && ::send(m_fd, packet.constData(), packet.size(), MSG_NOSIGNAL) == packet.size();
    }

    // Callers close from slots this socket's own notifiers are still dispatching, hence deleteLater.
    void close()
    {
        for (QSocketNotifier *notifier : {m_readNotifier, m_writeNotifier}) {
            if (notifier) {
                notifier->setEnabled(false);
                notifier->deleteLater();
            }
        }
        m_readNotifier = nullptr;
        m_writeNotifier = nullptr;
        if (m_fd >= 0)
            ::close(m_fd);
        m_fd = -1;
        m_connected = false;
    }

signals:
    void connected();
    void disconnected();
    void errorOccurred();
    // One AACP frame per signal: a SEQPACKET read never splits or joins them.
    void packetReceived(const QByteArray &packet);

private:
    // From <bluetooth/bluetooth.h> and <bluetooth/l2cap.h>, which the build does not otherwise need.
    static constexpr int BTPROTO_L2CAP = 0;
    static constexpr int SOL_L2CAP = 6;
    static constexpr int L2CAP_OPTIONS = 1;
    struct Options { quint16 omtu, imtu, flushTo; quint8 mode, fcs, maxTx; quint16 txwinSize; };
    struct Address { sa_family_t family; quint16 psm; quint8 bdaddr[6]; quint16 cid; quint8 bdaddrType; };

    void finishConnect()
    {
        int error = 0;
        socklen_t size = sizeof(error);
        getsockopt(m_fd, SOL_SOCKET, SO_ERROR, &error, &size);
        if (error != 0) {
            fail(error);
            return;
        }
        m_writeNotifier->setEnabled(false);
        m_writeNotifier->deleteLater();
        m_writeNotifier = nullptr;
        m_connected = true;
        m_readNotifier = new QSocketNotifier(m_fd, QSocketNotifier::Read, this);
        connect(m_readNotifier, &QSocketNotifier::activated, this, &ControlSocket::readPackets);
        emit connected();
    }

    void readPackets()
    {
        char buffer[RECEIVE_MTU];
        while (m_fd >= 0) {
            const ssize_t n = ::recv(m_fd, buffer, sizeof(buffer), 0);
            if (n > 0) {
                emit packetReceived(QByteArray(buffer, n));
                continue;
            }
            if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
                return;
            m_error = n == 0 ? QStringLiteral("closed by the AirPods") : QString::fromLocal8Bit(strerror(errno));
            close();
            emit disconnected();
            return;
        }
    }

    void fail(int error)
    {
        m_error = QString::fromLocal8Bit(strerror(error));
        close();
        emit errorOccurred();
    }

    int m_fd = -1;
    bool m_connected = false;
    QBluetoothAddress m_peer;
    QString m_error;
    QSocketNotifier *m_readNotifier = nullptr;
    QSocketNotifier *m_writeNotifier = nullptr;
};
