#include <QTest>
#include <QFile>
#include <QElapsedTimer>
#include <QTimer>
#include <QTemporaryDir>
#include "../media/conversationvolume.hpp"
#include "../airpods_packets.h"

class TestConversationalAwareness : public QObject
{
    Q_OBJECT

private slots:
    void softwareFadeCanReverseAndReset()
    {
        QTemporaryDir directory;
        QVERIFY(directory.isValid());
        const QByteArray previousPath = qgetenv("PATH");
        const QByteArray previousLog = qgetenv("CONVERSATION_VOLUME_TEST_LOG");
        struct RestoreEnvironment {
            QByteArray path, log;
            ~RestoreEnvironment() {
                qputenv("PATH", path);
                if (log.isNull()) qunsetenv("CONVERSATION_VOLUME_TEST_LOG");
                else qputenv("CONVERSATION_VOLUME_TEST_LOG", log);
            }
        } restore{previousPath, previousLog};
        const QString logPath = directory.filePath("commands");
        QFile executable(directory.filePath("pw-cli"));
        QVERIFY(executable.open(QIODevice::WriteOnly));
        executable.write(R"SH(#!/bin/sh
printf '%s\n' "$*" >> "$CONVERSATION_VOLUME_TEST_LOG"
if [ -f "$CONVERSATION_VOLUME_TEST_LOG.stall" ]; then sleep 0.25; fi
if [ -f "$CONVERSATION_VOLUME_TEST_LOG.fail" ]; then
    rm "$CONVERSATION_VOLUME_TEST_LOG.fail"
    echo 'Error: simulated write failure' >&2
    exit 1
fi
if [ "$4" = '{ volume: 1.000000 }' ] && [ -f "$CONVERSATION_VOLUME_TEST_LOG.fail_restore_always" ]; then
    echo 'Error: simulated persistent restoration failure' >&2
    exit 1
fi
if [ "$4" = '{ volume: 1.000000 }' ] && [ -f "$CONVERSATION_VOLUME_TEST_LOG.fail_restore" ]; then
    rm "$CONVERSATION_VOLUME_TEST_LOG.fail_restore"
    echo 'Error: simulated restoration failure' >&2
    exit 1
fi
printf '%s\n' "$*" >> "$CONVERSATION_VOLUME_TEST_LOG.applied"
)SH");
        executable.close();
        QVERIFY(executable.setPermissions(QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner));
        qputenv("PATH", directory.path().toUtf8() + ':' + previousPath);
        qputenv("CONVERSATION_VOLUME_TEST_LOG", logPath.toUtf8());
        auto commands = [&]() {
            QFile log(logPath);
            if (!log.open(QIODevice::ReadOnly)) return QByteArray();
            return log.readAll();
        };
        auto lastGain = [&]() {
            QFile applied(logPath + QStringLiteral(".applied"));
            if (!applied.open(QIODevice::ReadOnly)) return -1.0;
            const auto lines = applied.readAll().trimmed().split('\n');
            const auto fields = lines.last().split(' ');
            return fields.size() >= 6 ? fields[5].toDouble() : -1.0;
        };

        ConversationVolume volume;
        const QString sink = QStringLiteral("bluez_output.test.1");
        volume.setSpeaking(true, sink);
        volume.setSpeaking(false, sink);
        QTest::qWait(80);
        QVERIFY(commands().isEmpty());

        volume.setSpeaking(true, sink);
        QTest::qWait(250);
        QVERIFY(lastGain() > 0.008 && lastGain() < 1.0);
        volume.setSpeaking(true, sink);
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 0.008, 1200);
        QVERIFY(commands().count('\n') > 10);
        QVERIFY(!commands().contains("volumeRamp"));

        volume.setSpeaking(false, sink);
        QTest::qWait(150);
        const double risingGain = lastGain();
        QVERIFY(risingGain > 0.008 && risingGain < 1.0);
        volume.setSpeaking(true, sink);
        QTest::qWait(100);
        QVERIFY(lastGain() < risingGain);
        volume.reset();
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 1.0, 1200);
        const QByteArray afterReset = commands();
        QTest::qWait(700);
        QCOMPARE(commands(), afterReset);

        volume.setSpeaking(true, sink);
        QTest::qWait(100);
        QVERIFY(lastGain() < 1.0);
        const qsizetype beforeReplacement = commands().size();
        volume.setSpeaking(true, QStringLiteral("bluez_output.replacement.1"));
        QTRY_VERIFY(commands().mid(beforeReplacement).startsWith("set-param bluez_output.test.1 Props { volume: 1.000000 }\n"));
        volume.reset();
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 1.0, 1200);

        auto marker = [&](const QString &suffix) {
            QFile file(logPath + suffix);
            return file.open(QIODevice::WriteOnly);
        };
        QVERIFY(marker(QStringLiteral(".stall")));
        QElapsedTimer elapsed;
        elapsed.start();
        qint64 independentTimerAt = -1;
        QTimer::singleShot(30, &volume, [&] { independentTimerAt = elapsed.elapsed(); });
        volume.setSpeaking(true, sink);
        QTest::qWait(80);
        QVERIFY(independentTimerAt >= 0 && independentTimerAt < 200);
        QVERIFY(QFile::remove(logPath + QStringLiteral(".stall")));
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 0.008, 1200);

        QVERIFY(marker(QStringLiteral(".fail")));
        volume.setSpeaking(false, sink);
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 1.0, 1500);
        QVERIFY(!QFile::exists(logPath + QStringLiteral(".fail")));

        volume.setSpeaking(true, sink);
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 0.008, 1200);
        const qsizetype beforeFailure = commands().size();
        QVERIFY(marker(QStringLiteral(".fail_restore")));
        volume.reset();
        volume.setSpeaking(true, QStringLiteral("bluez_output.replacement.1"));
        QTRY_VERIFY(commands().mid(beforeFailure).count(
            "set-param bluez_output.test.1 Props { volume: 1.000000 }") == 2);
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 0.008, 1500);
        const QByteArray recovery = commands().mid(beforeFailure);
        QVERIFY(recovery.lastIndexOf("bluez_output.test.1") < recovery.indexOf("bluez_output.replacement.1"));
        volume.reset();
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 1.0, 1200);

        QVERIFY(marker(QStringLiteral(".stall")));
        QVERIFY(marker(QStringLiteral(".fail")));
        volume.setSpeaking(true, sink);
        QTest::qWait(80);
        const qsizetype beforeDelayedFailure = commands().size();
        QVERIFY(QFile::remove(logPath + QStringLiteral(".stall")));
        volume.setSpeaking(true, QStringLiteral("bluez_output.replacement.1"));
        QTRY_VERIFY(commands().mid(beforeDelayedFailure).contains("bluez_output.replacement.1"));
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 0.008, 1500);
        volume.reset();
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 1.0, 1200);

        // Reaching unity through a normal fade also completes the previous dirty cycle.
        volume.setSpeaking(true, sink);
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 0.008, 1200);
        QVERIFY(marker(QStringLiteral(".stall")));
        QVERIFY(marker(QStringLiteral(".fail_restore")));
        volume.reset();
        volume.setSpeaking(false, sink);
        QTRY_VERIFY(!QFile::exists(logPath + QStringLiteral(".fail_restore")));
        QVERIFY(QFile::remove(logPath + QStringLiteral(".stall")));
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 1.0, 1800);

        volume.setSpeaking(true, sink);
        QTRY_COMPARE_WITH_TIMEOUT(lastGain(), 0.008, 1200);
        const qsizetype beforePersistentFailure = commands().size();
        const QByteArray restoreCommand = "set-param bluez_output.test.1 Props { volume: 1.000000 }";
        constexpr int expectedRestoreAttempts = 2;
        constexpr int eventSettleMs = 100;
        constexpr int stalledWriteSettleMs = 400;
        QVERIFY(marker(QStringLiteral(".fail_restore_always")));
        QVERIFY(marker(QStringLiteral(".stall")));
        volume.reset();
        QTRY_COMPARE(commands().mid(beforePersistentFailure).count(restoreCommand), expectedRestoreAttempts);

        // Events during the last in-flight attempt must not replenish its budget.
        volume.setSpeaking(true, QStringLiteral("bluez_output.replacement.1"));
        volume.reset();
        QVERIFY(QFile::remove(logPath + QStringLiteral(".stall")));
        QTest::qWait(stalledWriteSettleMs);
        QCOMPARE(commands().mid(beforePersistentFailure).count(restoreCommand), expectedRestoreAttempts);
        const QByteArray afterExhaustion = commands();

        // Both event paths must remain bounded after persistent failure exhausts the budget.
        for (const QString &requestedSink : {sink, QStringLiteral("bluez_output.replacement.1")}) {
            for (bool speaking : {true, false}) {
                volume.setSpeaking(speaking, requestedSink);
                QTest::qWait(eventSettleMs);
                QCOMPARE(commands(), afterExhaustion);
                volume.reset();
                QTest::qWait(eventSettleMs);
                QCOMPARE(commands(), afterExhaustion);
            }
        }
        QCOMPARE(lastGain(), 0.008);
    }

    void conversationKeepsVolumeLoweredUntilEnd()
    {
        // The 01 -> 02 speech sequence must not restore volume on its second packet.
        for (int level : {0x01, 0x02}) {
            const auto speaking = AirPodsPackets::ConversationalAwareness::parseSpeaking(
                AirPodsPackets::ConversationalAwareness::DATA_HEADER + static_cast<char>(level));
            QVERIFY(speaking.has_value());
            QVERIFY(speaking.value());
        }
        for (int level : {0x03, 0x04, 0x05, 0x07, 0x00, 0xff}) {
            QVERIFY(!AirPodsPackets::ConversationalAwareness::parseSpeaking(
                AirPodsPackets::ConversationalAwareness::DATA_HEADER + static_cast<char>(level)).has_value());
        }
        for (int level : {0x06, 0x08, 0x09}) {
            const auto speaking = AirPodsPackets::ConversationalAwareness::parseSpeaking(
                AirPodsPackets::ConversationalAwareness::DATA_HEADER + static_cast<char>(level));
            QVERIFY(speaking.has_value());
            QVERIFY(!speaking.value());
        }
    }

    void rejectsMalformedPackets()
    {
        const QByteArray packet = QByteArray::fromHex("040004004b0002000102");
        for (int size = 0; size < packet.size(); ++size)
            QVERIFY(!AirPodsPackets::ConversationalAwareness::parseSpeaking(packet.left(size)).has_value());
        QVERIFY(!AirPodsPackets::ConversationalAwareness::parseSpeaking(packet + '\0').has_value());
        QByteArray wrongHeader = packet;
        wrongHeader[4] = 0x09;
        QVERIFY(!AirPodsPackets::ConversationalAwareness::parseSpeaking(wrongHeader).has_value());
    }
};

QTEST_GUILESS_MAIN(TestConversationalAwareness)
#include "tst_conversationalawareness.moc"
