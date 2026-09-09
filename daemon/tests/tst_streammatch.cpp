#include <QTest>
#include "../media/streammatch.hpp"

class TestStreamMatch : public QObject
{
    Q_OBJECT

private slots:
    void testIgnoredMediaRoles();
    void testClassifyStream();
    void testComputeCombinedMediaState();
};

void TestStreamMatch::testIgnoredMediaRoles()
{
    QVERIFY(isIgnoredMediaRole(QStringLiteral("event")));
    QVERIFY(isIgnoredMediaRole(QStringLiteral("notification")));
    QVERIFY(isIgnoredMediaRole(QStringLiteral("alert")));
    QVERIFY(isIgnoredMediaRole(QStringLiteral("test")));
    QVERIFY(isIgnoredMediaRole(QStringLiteral("announcement")));
    QVERIFY(isIgnoredMediaRole(QStringLiteral(" EVENT ")));
    QVERIFY(isIgnoredMediaRole(QStringLiteral("Notification")));

    QVERIFY(!isIgnoredMediaRole(QStringLiteral("")));
    QVERIFY(!isIgnoredMediaRole(QStringLiteral("music")));
    QVERIFY(!isIgnoredMediaRole(QStringLiteral("video")));
    QVERIFY(!isIgnoredMediaRole(QStringLiteral("game")));
}

void TestStreamMatch::testClassifyStream()
{
    // Active playback: uncorked, unmuted, standard/empty media role
    QCOMPARE(classifyStream(false, false, QStringLiteral("")), StreamPlaybackCategory::ActivePlayback);
    QCOMPARE(classifyStream(false, false, QStringLiteral("music")), StreamPlaybackCategory::ActivePlayback);
    QCOMPARE(classifyStream(false, false, QStringLiteral("video")), StreamPlaybackCategory::ActivePlayback);

    // Corked playback (explicit pause)
    QCOMPARE(classifyStream(true, false, QStringLiteral("")), StreamPlaybackCategory::CorkedPlayback);
    QCOMPARE(classifyStream(true, false, QStringLiteral("video")), StreamPlaybackCategory::CorkedPlayback);

    // Muted streams are ignored
    QCOMPARE(classifyStream(false, true, QStringLiteral("music")), StreamPlaybackCategory::Ignored);
    QCOMPARE(classifyStream(true, true, QStringLiteral("music")), StreamPlaybackCategory::Ignored);

    // Event/notification streams are ignored regardless of cork/mute
    QCOMPARE(classifyStream(false, false, QStringLiteral("event")), StreamPlaybackCategory::Ignored);
    QCOMPARE(classifyStream(false, false, QStringLiteral("notification")), StreamPlaybackCategory::Ignored);
    QCOMPARE(classifyStream(true, false, QStringLiteral("alert")), StreamPlaybackCategory::Ignored);
}

void TestStreamMatch::testComputeCombinedMediaState()
{
    // If MPRIS is playing, combined is Playing
    QCOMPARE(computeCombinedMediaState(CombinedMediaState::Playing, false, false), CombinedMediaState::Playing);
    QCOMPARE(computeCombinedMediaState(CombinedMediaState::Playing, true, false), CombinedMediaState::Playing);

    // If stream is playing without MPRIS (e.g. YouTube in Slack), combined is Playing
    QCOMPARE(computeCombinedMediaState(CombinedMediaState::Stopped, true, false), CombinedMediaState::Playing);
    QCOMPARE(computeCombinedMediaState(CombinedMediaState::Paused, true, false), CombinedMediaState::Playing);

    // If stream is explicitly corked/paused and MPRIS is stopped, combined is Paused
    QCOMPARE(computeCombinedMediaState(CombinedMediaState::Stopped, false, true), CombinedMediaState::Paused);

    // If MPRIS is paused and stream is stopped, combined is Paused
    QCOMPARE(computeCombinedMediaState(CombinedMediaState::Paused, false, false), CombinedMediaState::Paused);

    // If both stopped/idle, combined is Stopped
    QCOMPARE(computeCombinedMediaState(CombinedMediaState::Stopped, false, false), CombinedMediaState::Stopped);
}

QTEST_GUILESS_MAIN(TestStreamMatch)
#include "tst_streammatch.moc"
