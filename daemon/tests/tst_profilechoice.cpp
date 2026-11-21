#include <QtTest>

#include "../media/profilechoice.hpp"

// Priorities and names here are the ones minipc's AirPods card actually reports.
class TestProfileChoice : public QObject
{
    Q_OBJECT

private slots:
    void prefersAacWhateverThePriority();
    void unknownCodecsFallBackToPriority();
    void skipsHeadsetProfilesThatCarryASource();
    void skipsUnavailableProfiles();
    void skipsProfilesWithNoSink();
    void returnsEmptyWhenNothingQualifies();
    void readsTheCodecOutOfAProfileDescription();
    void readsTheCodecOutOfTheProfileNameToo();
};

static QVector<ProfileCandidate> airPodsCard()
{
    return {
        {"off", "Off", 0, true, 0, 0},
        {"a2dp-sink-sbc", "High Fidelity Playback (A2DP Sink, codec SBC)", 132, true, 1, 0},
        {"a2dp-sink-sbc_xq", "High Fidelity Playback (A2DP Sink, codec SBC-XQ)", 131, true, 1, 0},
        {"a2dp-sink", "High Fidelity Playback (A2DP Sink, codec AAC)", 133, true, 1, 0},
        {"headset-head-unit-cvsd", "Headset Head Unit (HSP/HFP, codec CVSD)", 5, true, 1, 1},
        {"headset-head-unit", "Headset Head Unit (HSP/HFP, codec MSBC)", 6, true, 1, 1},
    };
}

void TestProfileChoice::prefersAacWhateverThePriority()
{
    QCOMPARE(bestPlaybackProfile(airPodsCard()), QString("a2dp-sink"));
    // AAC still leads when PipeWire ranks it last.
    QVector<ProfileCandidate> aacRankedLast = {
        {"a2dp-sink-sbc", "High Fidelity Playback (A2DP Sink, codec SBC)", 133, true, 1, 0},
        {"a2dp-sink-sbc_xq", "High Fidelity Playback (A2DP Sink, codec SBC-XQ)", 132, true, 1, 0},
        {"a2dp-sink", "High Fidelity Playback (A2DP Sink, codec AAC)", 1, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(aacRankedLast), QString("a2dp-sink"));
    // Without AAC, SBC-XQ comes before SBC.
    QVector<ProfileCandidate> noAac = {
        {"a2dp-sink-sbc", "High Fidelity Playback (A2DP Sink, codec SBC)", 133, true, 1, 0},
        {"a2dp-sink-sbc_xq", "High Fidelity Playback (A2DP Sink, codec SBC-XQ)", 131, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(noAac), QString("a2dp-sink-sbc_xq"));
}

void TestProfileChoice::unknownCodecsFallBackToPriority()
{
    // An unknown codec scores 0 bitrate, so PipeWire's priority breaks the tie.
    QVector<ProfileCandidate> unknownCodecs = {
        {"vendor-alpha", "Vendor Playback (A2DP Sink, codec MYSTERY)", 12, true, 1, 0},
        {"vendor-beta", "Vendor Playback (A2DP Sink, codec LDAC)", 99, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(unknownCodecs), QString("vendor-beta"));

    // Same tie the other way round, so an order-sensitive comparison cannot pass both.
    QVector<ProfileCandidate> unknownCodecsReversed = {
        {"vendor-beta", "Vendor Playback (A2DP Sink, codec LDAC)", 99, true, 1, 0},
        {"vendor-alpha", "Vendor Playback (A2DP Sink, codec MYSTERY)", 12, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(unknownCodecsReversed), QString("vendor-beta"));

    // Two candidates equal on bitrate and on priority must resolve the same way either way round.
    QVector<ProfileCandidate> equalOnBoth = {
        {"first-seen", "Vendor Playback (A2DP Sink, codec MYSTERY)", 50, true, 1, 0},
        {"second-seen", "Vendor Playback (A2DP Sink, codec ENIGMA)", 50, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(equalOnBoth), QString("first-seen"));
    QVector<ProfileCandidate> equalOnBothReversed = {
        {"second-seen", "Vendor Playback (A2DP Sink, codec ENIGMA)", 50, true, 1, 0},
        {"first-seen", "Vendor Playback (A2DP Sink, codec MYSTERY)", 50, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(equalOnBothReversed), QString("second-seen"));

    // AAC's own row: it must outscore an unknown codec that outranks it on priority.
    QVector<ProfileCandidate> aacVersusUnknown = {
        {"vendor-mystery", "Vendor Playback (A2DP Sink, codec MYSTERY)", 500, true, 1, 0},
        {"a2dp-sink", "High Fidelity Playback (A2DP Sink, codec AAC)", 133, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(aacVersusUnknown), QString("a2dp-sink"));

    // A known codec always beats an unknown one, whatever the names say.
    QVector<ProfileCandidate> knownVersusUnknown = {
        {"zzz-vendor", "Vendor Playback (A2DP Sink, codec MYSTERY)", 500, true, 1, 0},
        {"aaa-vendor", "High Fidelity Playback (A2DP Sink, codec AAC)", 1, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(knownVersusUnknown), QString("aaa-vendor"));
}

void TestProfileChoice::skipsHeadsetProfilesThatCarryASource()
{
    // Best codec and top priority here, so only the source term can reject the headset.
    QVector<ProfileCandidate> headsetRanksHighest = {
        {"headset-head-unit", "Headset Head Unit (HSP/HFP, codec SBC-XQ)", 200, true, 1, 1},
        {"a2dp-sink", "High Fidelity Playback (A2DP Sink, codec AAC)", 133, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(headsetRanksHighest), QString("a2dp-sink"));
}

void TestProfileChoice::skipsUnavailableProfiles()
{
    QVector<ProfileCandidate> aacUnavailable = airPodsCard();
    for (ProfileCandidate &c : aacUnavailable) {
        if (c.name == "a2dp-sink") c.available = false;
    }
    QCOMPARE(bestPlaybackProfile(aacUnavailable), QString("a2dp-sink-sbc_xq"));
}

void TestProfileChoice::skipsProfilesWithNoSink()
{
    // `off` outranks nothing, but a zero-sink profile must never be chosen.
    // `off` carries the best codec string here so that only the sink term can reject it.
    QVector<ProfileCandidate> offRanksHighest = {
        {"off", "Off (A2DP Sink, codec SBC-XQ)", 500, true, 0, 0},
        {"a2dp-sink", "High Fidelity Playback (A2DP Sink, codec AAC)", 133, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(offRanksHighest), QString("a2dp-sink"));
    QCOMPARE(codecFromDescription("Headset Head Unit (HSP/HFP, codec MSBC)"), QString("MSBC"));
    QVERIFY(codecFromDescription("Off").isEmpty());
}

void TestProfileChoice::returnsEmptyWhenNothingQualifies()
{
    QVector<ProfileCandidate> headsetOnly = {
        {"off", "Off", 0, true, 0, 0},
        {"headset-head-unit", "Headset Head Unit (HSP/HFP, codec MSBC)", 6, true, 1, 1},
    };
    QVERIFY(bestPlaybackProfile(headsetOnly).isEmpty());
    QVERIFY(bestPlaybackProfile({}).isEmpty());
}

void TestProfileChoice::readsTheCodecOutOfAProfileDescription()
{
    // The profile name hides the codec, so the description is the only place to read it.
    QCOMPARE(codecFromDescription(profileDescription(airPodsCard(), "a2dp-sink")), QString("AAC"));
    QCOMPARE(codecFromDescription(profileDescription(airPodsCard(), "a2dp-sink-sbc_xq")), QString("SBC-XQ"));
    QVERIFY(profileDescription(airPodsCard(), "no-such-profile").isEmpty());

    // A description that never closes its bracket, one with no codec at all, and nothing.
    QCOMPARE(codecFromDescription("A2DP Sink, codec SBC-XQ"), QString("SBC-XQ"));
    QVERIFY(codecFromDescription("High Fidelity Playback (A2DP Sink)").isEmpty());
    QCOMPARE(codecFromDescription("High Fidelity Playback (A2DP Sink, codec SBC-XQ )"), QString("SBC-XQ"));
    QVERIFY(codecFromDescription("A2DP Sink, codec )").isEmpty());
    QVERIFY(codecFromDescription(QString()).isEmpty());
}

void TestProfileChoice::readsTheCodecOutOfTheProfileNameToo()
{
    // Descriptions are translated by the server, names are identifiers, so the name comes first.
    QCOMPARE(codecFromProfileName("a2dp-sink-sbc_xq"), QString("SBC-XQ"));
    QCOMPARE(codecFromProfileName("a2dp-sink-sbc"), QString("SBC"));
    QCOMPARE(codecFromProfileName("a2dp-sink-aac"), QString("AAC"));
    QVERIFY(codecFromProfileName("a2dp-sink").isEmpty());
    QVERIFY(codecFromProfileName("headset-head-unit").isEmpty());
    QVERIFY(codecFromProfileName("off").isEmpty());

    // Under a translated description the bare profile is the codec no suffixed sibling carries.
    QVector<ProfileCandidate> translated = {
        {"a2dp-sink", "Reproduccion de alta fidelidad (A2DP Sink)", 1, true, 1, 0},
        {"a2dp-sink-sbc", "Reproduccion de alta fidelidad (A2DP Sink)", 133, true, 1, 0},
        {"a2dp-sink-sbc_xq", "Reproduccion de alta fidelidad (A2DP Sink)", 131, true, 1, 0},
    };
    QCOMPARE(bestPlaybackProfile(translated), QString("a2dp-sink"));
}

QTEST_GUILESS_MAIN(TestProfileChoice)
#include "tst_profilechoice.moc"
