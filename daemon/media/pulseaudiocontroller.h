#pragma once

#include <QObject>
#include <QString>
#include <atomic>
#include <pulse/pulseaudio.h>

#include "capturematch.hpp"
#include "profilechoice.hpp"

class PulseAudioController : public QObject
{
    Q_OBJECT
    // PulseAudioController owns a pa_threaded_mainloop + pa_context.
    // Both are reference-counted resources that get unref'd in the
    // destructor. Copying would double-free at end of scope and
    // corrupt PulseAudio internals. Q_DISABLE_COPY (the QObject-aware
    // form of =delete on copy ctor + assign) prevents accidental
    // copies — a real concern because the surrounding tray-app code
    // passes this object around by reference + holds it as a raw
    // pointer member, so a value-copy slip would compile silently
    // without the macro.
    Q_DISABLE_COPY(PulseAudioController)

public:
    explicit PulseAudioController(QObject *parent = nullptr);
    ~PulseAudioController();

    bool initialize();
    QString getDefaultSink();
    QString getSinkForDevice(const QString &macAddress);
    bool setDefaultSink(const QString &sinkName);
    int getSinkVolume(const QString &sinkName);
    bool setSinkVolume(const QString &sinkName, int volumePercent);

    // Enable AVRCP 5%-grid volume snap on the named sink. PA fires a
    // SINK_CHANGE subscribe event on every sink-volume-changed
    // (including BlueZ AVRCP AbsoluteVolume writes from a stem-swipe
    // on AirPods). The handler reads the new volume, snaps to the
    // nearest 5% step via snapToGrid, and re-sets the sink volume
    // when the snap differs AND the change wasn't daemon-initiated.
    //
    // AirPods AVRCP uses 15-step granularity (1/15 ~= 6.67%) which
    // produces an off-grid sequence (7/14/19/27/...) when the user
    // swipes the stem. Snapping back to the 5% grid (5/15/20/...)
    // matches the Quickshell keyboard-volume step. Idempotent +
    // feedback-loop-guarded via m_expectedVolume.
    //
    // sinkName should match `pactl list short sinks` (typically
    // `bluez_output.<MAC>.1`). Empty string disables the snap.
    void enableVolumeSnap(const QString &sinkName, int stepPercent = 5);

    bool setCardProfile(const QString &cardName, const QString &profileName);
    QString getCardNameForDevice(const QString &macAddress);
    bool isProfileAvailable(const QString &cardName, const QString &profileName);
    QString getActiveCardProfile(const QString &cardName);

    // Live while any stream is attached to this address's mic, which is what a call holds.
    CaptureState captureState(const QString &macAddress);
    // Every profile this card reports, for the pure choice in profilechoice.hpp.
    QVector<ProfileCandidate> getCardProfiles(const QString &cardName);

private:
    pa_threaded_mainloop *m_mainloop = nullptr;
    pa_context *m_context = nullptr;
    bool m_initialized = false;

    // AVRCP 5%-grid snap state. m_snapSinkName empty disables.
    // m_expectedVolume tracks the last daemon-initiated set so the
    // subscribe callback can ignore its own setSinkVolume echo and
    // avoid an infinite loop. Atomic because the callback runs on
    // the PA mainloop thread while setSinkVolume runs on whichever
    // thread called it (typically Qt main).
    QString m_snapSinkName;
    int m_snapStep = 5;
    std::atomic<int> m_expectedVolume{-1};

    static void contextStateCallback(pa_context *c, void *userdata);
    static void sinkInfoCallback(pa_context *c, const pa_sink_info *info, int eol, void *userdata);
    static void cardInfoCallback(pa_context *c, const pa_card_info *info, int eol, void *userdata);
    static void serverInfoCallback(pa_context *c, const pa_server_info *info, void *userdata);

    // PA subscribe callback for SINK events; defers to
    // snapSinkInfoCallback to fetch volume because subscribeCallback
    // only gets event-type + sink-index, not the new volume.
    static void subscribeCallback(pa_context *c, pa_subscription_event_type_t type, uint32_t idx, void *userdata);
    static void snapSinkInfoCallback(pa_context *c, const pa_sink_info *info, int eol, void *userdata);

    bool waitForOperation(pa_operation *op);
};
