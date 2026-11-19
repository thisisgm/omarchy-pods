#include <QCoreApplication>
#include <QFileInfo>
#include <QLocalSocket>
#include <QTextStream>

#include "ipcpath.hpp"

int main(int argc, char *argv[]) {
    QCoreApplication app(argc, argv);

    if (argc < 2) {
        // Self-name: use argv[0] basename so a symlink (openpods-ctl)
        // shows the right usage line. Falls back to "openpods-ctl" if
        // QCoreApplication can't tell us its file path.
        const QString self = QFileInfo(QCoreApplication::applicationFilePath()).fileName();
        const QString prog = self.isEmpty() ? QStringLiteral("openpods-ctl") : self;
        QTextStream(stderr) << "Usage: " << prog << " <command>\n"
                            << "Commands:\n"
                            << "  status              Print one-line JSON status snapshot to stdout\n"
                            << "  noise:off           Disable noise control\n"
                            << "  noise:anc           Enable Active Noise Cancellation\n"
                            << "  noise:transparency  Enable Transparency mode\n"
                            << "  noise:adaptive      Enable Adaptive mode\n"
                            << "  noise:cycle         Advance to the next mode (Off->ANC->Trans->Adaptive)\n"
                            << "  ear:off             Disable automatic ear-detection auto-pause/resume\n"
                            << "  ear:one             Pause when either pod is removed (default)\n"
                            << "  ear:both            Pause only when both pods are removed\n"
                            << "  forget              Run `bluetoothctl remove` on the connected device\n"
                            << "  ca:on               Enable Conversation Awareness (Pro2 only)\n"
                            << "  ca:off              Disable Conversation Awareness\n"
                            << "  disconnect          bluetoothctl disconnect on the paired AirPods\n"
                            << "  connect             bluetoothctl connect on the paired AirPods\n"
                            << "  media-connect:on    Enable Chrome/Chromium/Spotify-triggered connections (default)\n"
                            << "  media-connect:off   Disable playback-triggered connections\n"
                            << "  adaptive:N          Set Adaptive Noise level 0-100 (Pro2/Pro3, only while noise_mode=Adaptive)\n"
                            << "  onebud:on           Enable One-Bud ANC (Pro2+: keep ANC active with only one pod in)\n"
                            << "  onebud:off          Disable One-Bud ANC\n";
        return 1;
    }

    const QByteArray cmd = QByteArray(argv[1]);
    // reopen is the only other verb the daemon can refuse, and its refusal is a reply.
    const bool wantsStatus = (cmd == "status");
    const bool wantsReply = wantsStatus || (cmd == "reopen");

    const QString ipcPath = OpenPods::Ipc::socketPath();
    if (ipcPath.isEmpty()) {
        QTextStream(stderr) << "XDG_RUNTIME_DIR is unset; cannot locate the daemon socket\n";
        return 1;
    }

    QLocalSocket socket;
    socket.connectToServer(ipcPath);

    if (!socket.waitForConnected(500)) {
        QTextStream(stderr) << "Could not connect to OpenPods daemon at " << ipcPath
                            << " (is it running?)\n";
        return 1;
    }

    socket.write(cmd);
    socket.flush();
    // 200ms was tight: on a slow host (especially under valgrind/ASAN)
    // the daemon hadn't drained the write before disconnectFromServer
    // tore the socket down, silently dropping the command. 500ms gives
    // headroom without making the CLI feel laggy.
    socket.waitForBytesWritten(500);

    if (wantsReply) {
        // Daemon writes one JSON line then half-closes. Wait briefly
        // for the response before tearing the socket down, otherwise
        // the read silently returns empty.
        if (!socket.waitForReadyRead(1000)) {
            // A windowed daemon opens the window and closes the socket without answering, so that close is its yes.
            if (cmd == "reopen" && socket.state() != QLocalSocket::ConnectedState) {
                socket.disconnectFromServer();
                return 0;
            }
            QTextStream(stderr) << "Timed out waiting for a reply to " << cmd << "\n";
            socket.disconnectFromServer();
            return 1;
        }
        const QByteArray reply = socket.readAll();
        // A headless daemon refuses reopen with an error line rather than a status object.
        if (reply.startsWith("error:")) {
            QTextStream(stderr) << QString::fromUtf8(reply);
            socket.disconnectFromServer();
            return 1;
        }
        QTextStream(stdout) << QString::fromUtf8(reply);
    }

    socket.disconnectFromServer();
    return 0;
}
