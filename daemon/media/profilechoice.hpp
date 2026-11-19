#pragma once

#include <QString>
#include <QVector>

// Free of PulseAudio types so the choice below is testable without a sound server.
struct ProfileCandidate {
    QString name;
    QString description;
    int priority = 0;
    bool available = false;
    unsigned sinks = 0;
    unsigned sources = 0;
};

// corner: only these three reach a playback profile; MSBC and CVSD belong to the headset ones.
inline int codecBitrateKbps(const QString &codec)
{
    if (codec == "SBC-XQ") return 453;
    if (codec == "SBC") return 328;
    if (codec == "AAC") return 256;
    return 0;
}

// Sample input: "High Fidelity Playback (A2DP Sink, codec SBC-XQ)"
inline QString codecFromDescription(const QString &description)
{
    static const QString marker = "codec ";
    const int at = description.indexOf(marker);
    if (at < 0) {
        return QString();
    }
    const QString rest = description.mid(at + marker.length());
    const int end = rest.indexOf(')');
    return (end >= 0 ? rest.left(end) : rest).trimmed();
}

// Sample input: "a2dp-sink-sbc_xq". Names are identifiers, so they survive a translated locale.
inline QString codecFromProfileName(const QString &name)
{
    const int dash = name.lastIndexOf('-');
    if (dash < 0) {
        return QString();
    }
    const QString suffix = name.mid(dash + 1);
    // The bare a2dp-sink carries no codec suffix, and a headset profile's tail is not one either.
    if (suffix == "sink" || suffix == "source" || suffix == "unit" || suffix == "headset") {
        return QString();
    }
    return suffix.toUpper().replace('_', '-');
}

// Description PulseAudio gave this profile, empty when the card does not have it.
inline QString profileDescription(const QVector<ProfileCandidate> &candidates, const QString &name)
{
    for (const ProfileCandidate &c : candidates) {
        if (c.name == name) {
            return c.description;
        }
    }
    return QString();
}

// A profile carrying a source is the headset mic path, never the playback one.
inline QString bestPlaybackProfile(const QVector<ProfileCandidate> &candidates)
{
    QString best;
    int bestBitrate = -1;
    int bestPriority = -1;
    for (const ProfileCandidate &c : candidates) {
        if (!c.available || c.sinks == 0 || c.sources > 0) {
            continue;
        }
        // The name survives a translated locale, so it is tried first and the description backs it up.
        int bitrate = codecBitrateKbps(codecFromProfileName(c.name));
        if (bitrate == 0) {
            bitrate = codecBitrateKbps(codecFromDescription(c.description));
        }
        if (bitrate > bestBitrate || (bitrate == bestBitrate && c.priority > bestPriority)) {
            bestBitrate = bitrate;
            bestPriority = c.priority;
            best = c.name;
        }
    }
    return best;
}
