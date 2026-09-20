#include <QtTest>
#include "media/mediaconnectpolicy.hpp"
#include "media/playbackplayers.hpp"

class MediaConnectPolicyTest : public QObject
{
    Q_OBJECT
private slots:
    void malformedStatusDoesNotRearmEpisode()
    {
        MediaConnectPolicy policy;
        policy.observe(0, true, true);
        policy.observe(1000, playerIsPlaying("Buffering"), false);
        policy.observe(12000, playerIsPlaying(""), false);
        QVERIFY(!policy.observe(13000, true, false));
        QVERIFY(!policy.connectionStillWanted());
        QCOMPARE(playerIsPlaying("Playing"), std::optional<bool>(true));
        QCOMPARE(playerIsPlaying("Paused"), std::optional<bool>(false));
        QCOMPARE(playerIsPlaying("Stopped"), std::optional<bool>(false));
    }

    void supportedPlayers_data()
    {
        QTest::addColumn<QString>("service");
        QTest::addColumn<bool>("supported");
        QTest::newRow("chrome") << QStringLiteral("org.mpris.MediaPlayer2.google-chrome") << true;
        QTest::newRow("chromium-instance") << QStringLiteral("org.mpris.MediaPlayer2.chromium.instance123") << true;
        QTest::newRow("spotify") << QStringLiteral("org.mpris.MediaPlayer2.spotify") << true;
        QTest::newRow("spotify-instance") << QStringLiteral("org.mpris.MediaPlayer2.spotify.instance123") << true;
        QTest::newRow("unrelated-player") << QStringLiteral("org.mpris.MediaPlayer2.vlc") << false;
        QTest::newRow("similar-name") << QStringLiteral("org.mpris.MediaPlayer2.spotifyd") << false;
        QTest::newRow("not-mpris") << QStringLiteral("com.spotify.Client") << false;
    }

    void supportedPlayers()
    {
        QFETCH(QString, service);
        QFETCH(bool, supported);
        QCOMPARE(isAutoConnectPlayer(service), supported);
    }

    void firstPlaybackRequestsImmediately()
    {
        MediaConnectPolicy policy;
        QVERIFY(!policy.observe(0, false, false));
        QVERIFY(policy.observe(1000, true, false));
    }

    void unknownPlaybackDoesNotRearmEpisode()
    {
        MediaConnectPolicy policy;
        policy.observe(0, true, true);
        policy.observe(1000, std::nullopt, false);
        policy.observe(12000, std::nullopt, false);
        QVERIFY(!policy.observe(13000, true, false));
        QVERIFY(!policy.observe(15000, true, false));
    }

    void unknownPlaybackCancelsPendingLookup()
    {
        MediaConnectPolicy policy;
        QVERIFY(policy.observe(0, true, false));
        policy.observe(1, std::nullopt, false);
        QVERIFY(!policy.connectionStillWanted());
    }

    void slowFailureWaitsBeforeRetry()
    {
        MediaConnectPolicy policy;
        QVERIFY(policy.observe(0, true, false));
        QVERIFY(!policy.observe(17000, true, false, true));
        policy.connectionFinished(20000);
        QVERIFY(!policy.observe(20001, true, false));
        QVERIFY(!policy.observe(34999, true, false));
        QVERIFY(policy.observe(35000, true, false));
    }

    void idleConnectionEventsDoNotSuppressNextPlayback()
    {
        MediaConnectPolicy policy;
        policy.satisfy();
        QVERIFY(policy.observe(0, true, false));
    }

    void disconnectDuringLookupCancelsRequest()
    {
        MediaConnectPolicy policy;
        QVERIFY(policy.observe(0, true, false));
        QVERIFY(policy.connectionStillWanted());
        policy.satisfy();
        QVERIFY(!policy.connectionStillWanted());
    }

    void boundedRetries()
    {
        MediaConnectPolicy policy;
        QVERIFY(policy.observe(0, true, false));
        QVERIFY(!policy.observe(14999, true, false));
        QVERIFY(policy.observe(15000, true, false));
        QVERIFY(!policy.observe(120000, true, false));
    }

    void pauseCancelsPendingLookup()
    {
        MediaConnectPolicy policy;
        QVERIFY(policy.observe(0, true, false));
        QVERIFY(!policy.observe(1000, false, false));
        QVERIFY(!policy.connectionStillWanted());
        QVERIFY(!policy.observe(2000, true, false));
    }

    void manualDisconnectAndBriefPauseDoNotReclaim()
    {
        MediaConnectPolicy policy;
        QVERIFY(!policy.observe(0, true, true));
        QVERIFY(!policy.observe(2000, true, false));
        QVERIFY(!policy.observe(3000, false, false));
        QVERIFY(!policy.observe(5000, true, false));
        QVERIFY(!policy.observe(7000, true, false));
        QVERIFY(!policy.observe(8000, false, false));
        QVERIFY(policy.observe(18000, true, false));
    }

    void cooldownSurvivesNewEpisode()
    {
        MediaConnectPolicy policy;
        QVERIFY(policy.observe(0, true, false));
        policy.observe(3000, false, false);
        policy.observe(14000, true, false);
        QVERIFY(!policy.observe(16000, true, false));
        QVERIFY(!policy.observe(59999, true, false));
        QVERIFY(policy.observe(60000, true, false));
    }

    void pendingRequestDoesNotConsumeRetry()
    {
        MediaConnectPolicy policy;
        QVERIFY(policy.observe(0, true, false));
        QVERIFY(!policy.observe(17000, true, false, true));
        QVERIFY(policy.observe(18000, true, false));
        policy.satisfy();
        QVERIFY(!policy.observe(90000, true, false));
    }
};

QTEST_GUILESS_MAIN(MediaConnectPolicyTest)
#include "tst_mediaconnectpolicy.moc"
