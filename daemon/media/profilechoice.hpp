#pragma once

#include <QString>
#include <QStringList>
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

// AAC is the codec Apple itself uses with AirPods and the lightest on a link the hi-res mic
// shares, so it leads; SBC-XQ and SBC follow. MSBC and CVSD belong to the headset profiles.
inline int codecPreference(const QString &codec)
{
    if (codec == "AAC") return 3;
    if (codec == "SBC-XQ") return 2;
    if (codec == "SBC") return 1;
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

// PipeWire names one A2DP profile bare ("a2dp-sink") and suffixes the codec on the rest, so under
// a translated description the bare one is the known codec that no suffixed sibling carries.
inline QString codecOfBareA2dpSink(const QVector<ProfileCandidate> &candidates)
{
    QStringList missing = {"AAC", "SBC-XQ", "SBC"};
    for (const ProfileCandidate &c : candidates) {
        if (c.name.startsWith("a2dp-sink-")) {
            missing.removeAll(codecFromProfileName(c.name));
        }
    }
    return missing.size() == 1 ? missing.first() : QString();
}

// A profile carrying a source is the headset mic path, never the playback one.
inline QString bestPlaybackProfile(const QVector<ProfileCandidate> &candidates)
{
    QString best;
    int bestPreference = -1;
    int bestPriority = -1;
    for (const ProfileCandidate &c : candidates) {
        if (!c.available || c.sinks == 0 || c.sources > 0) {
            continue;
        }
        // The name survives a translated locale, so it is tried first and the description backs it up.
        int preference = codecPreference(codecFromProfileName(c.name));
        if (preference == 0) {
            preference = codecPreference(codecFromDescription(c.description));
        }
        if (preference == 0 && c.name == "a2dp-sink") {
            preference = codecPreference(codecOfBareA2dpSink(candidates));
        }
        if (preference > bestPreference || (preference == bestPreference && c.priority > bestPriority)) {
            bestPreference = preference;
            bestPriority = c.priority;
            best = c.name;
        }
    }
    return best;
}
