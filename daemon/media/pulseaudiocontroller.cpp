#include "pulseaudiocontroller.h"
#include "capturematch.hpp"
#include "profilechoice.hpp"
#include "logger.h"
#include "../snaptogrid.hpp"
#include <QElapsedTimer>
#include <QThread>

PulseAudioController::PulseAudioController(QObject *parent)
    : QObject(parent)
{
}

PulseAudioController::~PulseAudioController()
{
    // Order matters: stop the threaded mainloop *first* so no callback can
    // run while we tear down the context underneath it.
    if (m_mainloop) {
        pa_threaded_mainloop_stop(m_mainloop);
    }
    if (m_context) {
        pa_context_set_state_callback(m_context, nullptr, nullptr);
        pa_context_disconnect(m_context);
        pa_context_unref(m_context);
        m_context = nullptr;
    }
    if (m_mainloop) {
        pa_threaded_mainloop_free(m_mainloop);
        m_mainloop = nullptr;
    }
    m_initialized = false;
}

bool PulseAudioController::initialize()
{
    m_mainloop = pa_threaded_mainloop_new();
    if (!m_mainloop)
    {
        LOG_ERROR("Failed to create PulseAudio mainloop");
        return false;
    }

    pa_mainloop_api *api = pa_threaded_mainloop_get_api(m_mainloop);
    m_context = pa_context_new(api, "OpenPods");
    if (!m_context)
    {
        LOG_ERROR("Failed to create PulseAudio context");
        return false;
    }

    pa_context_set_state_callback(m_context, contextStateCallback, this);
    
    if (pa_threaded_mainloop_start(m_mainloop) < 0)
    {
        LOG_ERROR("Failed to start PulseAudio mainloop");
        return false;
    }

    pa_threaded_mainloop_lock(m_mainloop);
    
    if (pa_context_connect(m_context, nullptr, PA_CONTEXT_NOFLAGS, nullptr) < 0)
    {
        LOG_ERROR("Failed to connect to PulseAudio");
        pa_threaded_mainloop_unlock(m_mainloop);
        return false;
    }

    // Wait for context to be ready, with a hard cap so a broken daemon
    // can't freeze startup. pa_threaded_mainloop_wait drops the lock and
    // wakes on the next signal; cap to ~5s by counting iterations.
    QElapsedTimer timer; timer.start();
    constexpr qint64 kInitTimeoutMs = 5000;
    while (pa_context_get_state(m_context) != PA_CONTEXT_READY)
    {
        if (!PA_CONTEXT_IS_GOOD(pa_context_get_state(m_context)))
        {
            LOG_ERROR("PulseAudio context failed");
            pa_threaded_mainloop_unlock(m_mainloop);
            return false;
        }
        if (timer.elapsed() > kInitTimeoutMs) {
            LOG_ERROR("PulseAudio context did not become ready in 5s");
            pa_threaded_mainloop_unlock(m_mainloop);
            return false;
        }
        pa_threaded_mainloop_wait(m_mainloop);
    }

    // Register sink-event subscription. Fires for every sink change
    // including AVRCP AbsoluteVolume writes from the AirPods stem.
    // snapSinkInfoCallback (invoked indirectly by subscribeCallback)
    // does the actual 5%-grid snap when m_snapSinkName is set.
    // Subscribe-mask covers SINK events only; we don't care about
    // sink-input / source / card / etc. for the snap path.
    pa_context_set_subscribe_callback(m_context, &PulseAudioController::subscribeCallback, this);
    pa_operation *subOp = pa_context_subscribe(
        m_context, PA_SUBSCRIPTION_MASK_SINK, nullptr, nullptr);
    if (subOp) pa_operation_unref(subOp);

    pa_threaded_mainloop_unlock(m_mainloop);
    m_initialized = true;
    LOG_INFO("PulseAudio controller initialized");
    return true;
}

void PulseAudioController::contextStateCallback(pa_context *c, void *userdata)
{
    PulseAudioController *controller = static_cast<PulseAudioController*>(userdata);
    if (!controller || !controller->m_mainloop) return;
    const pa_context_state_t st = pa_context_get_state(c);
    if (st == PA_CONTEXT_FAILED || st == PA_CONTEXT_TERMINATED) {
        LOG_WARN("PulseAudio context state changed to " << static_cast<int>(st));
    }
    pa_threaded_mainloop_signal(controller->m_mainloop, 0);
}

QString PulseAudioController::getDefaultSink()
{
    if (!m_initialized) return QString();

    struct CallbackData {
        QString sinkName;
        pa_threaded_mainloop *mainloop = nullptr;
    } data;
    data.mainloop = m_mainloop;

    auto callback = [](pa_context *c, const pa_server_info *info, void *userdata) {
        CallbackData *d = static_cast<CallbackData*>(userdata);
        if (info && info->default_sink_name)
        {
            d->sinkName = QString::fromUtf8(info->default_sink_name);
        }
        pa_threaded_mainloop_signal(d->mainloop, 0);
    };

    pa_threaded_mainloop_lock(m_mainloop);
    pa_operation *op = pa_context_get_server_info(m_context, callback, &data);
    if (op)
    {
        waitForOperation(op);
        pa_operation_unref(op);
    }
    pa_threaded_mainloop_unlock(m_mainloop);

    return data.sinkName;
}

QString PulseAudioController::getSinkForDevice(const QString &macAddress)
{
    if (!m_initialized || macAddress.isEmpty()) return QString();

    struct CallbackData {
        QString mac;
        QString sinkName;
        pa_threaded_mainloop *mainloop = nullptr;
    } data;
    data.mac = macAddress;
    data.mainloop = m_mainloop;

    auto callback = [](pa_context *c, const pa_sink_info *info, int eol, void *userdata) {
        CallbackData *d = static_cast<CallbackData*>(userdata);
        if (eol != 0) {
            pa_threaded_mainloop_signal(d->mainloop, 0);
            return;
        }
        if (info && info->name) {
            QString name = QString::fromUtf8(info->name);
            if (name.contains(d->mac, Qt::CaseInsensitive)) {
                d->sinkName = name;
            }
        }
    };

    pa_threaded_mainloop_lock(m_mainloop);
    pa_operation *op = pa_context_get_sink_info_list(m_context, callback, &data);
    if (op) {
        waitForOperation(op);
        pa_operation_unref(op);
    }
    pa_threaded_mainloop_unlock(m_mainloop);

    return data.sinkName;
}

bool PulseAudioController::setDefaultSink(const QString &sinkName)
{
    if (!m_initialized || sinkName.isEmpty()) return false;

    struct CallbackData {
        pa_threaded_mainloop *mainloop = nullptr;
        bool success = false;
    } data;
    data.mainloop = m_mainloop;

    pa_threaded_mainloop_lock(m_mainloop);

    auto successCallback = [](pa_context *c, int success, void *userdata) {
        CallbackData *d = static_cast<CallbackData*>(userdata);
        d->success = (success != 0);
        pa_threaded_mainloop_signal(d->mainloop, 0);
    };

    pa_operation *op = pa_context_set_default_sink(m_context, sinkName.toUtf8().constData(), successCallback, &data);
    bool ok = waitForOperation(op);
    if (op) pa_operation_unref(op);
    pa_threaded_mainloop_unlock(m_mainloop);

    return ok && data.success;
}

int PulseAudioController::getSinkVolume(const QString &sinkName)
{
    if (!m_initialized) return -1;

    struct CallbackData {
        int volume = -1;
        QString targetSink;
        pa_threaded_mainloop *mainloop = nullptr;
    } data;
    data.targetSink = sinkName;
    data.mainloop = m_mainloop;

    auto callback = [](pa_context *c, const pa_sink_info *info, int eol, void *userdata) {
        CallbackData *d = static_cast<CallbackData*>(userdata);
        // PulseAudio reports a failed query with a negative eol, which must wake the waiter too.
        if (eol != 0)
        {
            pa_threaded_mainloop_signal(d->mainloop, 0);
            return;
        }
        if (info && QString::fromUtf8(info->name) == d->targetSink)
        {
            d->volume = (pa_cvolume_avg(&info->volume) * 100) / PA_VOLUME_NORM;
            pa_threaded_mainloop_signal(d->mainloop, 0);
        }
    };

    pa_threaded_mainloop_lock(m_mainloop);
    pa_operation *op = pa_context_get_sink_info_by_name(m_context, sinkName.toUtf8().constData(), callback, &data);
    if (op)
    {
        waitForOperation(op);
        pa_operation_unref(op);
    }
    pa_threaded_mainloop_unlock(m_mainloop);

    return data.volume;
}

bool PulseAudioController::setSinkVolume(const QString &sinkName, int volumePercent)
{
    if (!m_initialized) return false;

    // Track expected volume so the subscribe callback can ignore the
    // SINK_CHANGE event echo from this very call (otherwise the snap
    // callback fires on our own set, computes snapToGrid(N)==N for an
    // on-grid value, and silently no-ops — but for an off-grid value
    // we'd loop forever). Update for the snap-target sink only; other
    // sinks don't need the feedback-loop guard.
    if (sinkName == m_snapSinkName) {
        m_expectedVolume.store(volumePercent, std::memory_order_relaxed);
    }

    pa_cvolume volume;
    pa_cvolume_set(&volume, 2, (volumePercent * PA_VOLUME_NORM) / 100);

    pa_threaded_mainloop_lock(m_mainloop);

    auto successCallback = [](pa_context *c, int success, void *userdata) {
        pa_threaded_mainloop *mainloop = static_cast<pa_threaded_mainloop*>(userdata);
        pa_threaded_mainloop_signal(mainloop, 0);
    };

    pa_operation *op = pa_context_set_sink_volume_by_name(m_context, sinkName.toUtf8().constData(), &volume, successCallback, m_mainloop);

    bool success = waitForOperation(op);
    if (op) pa_operation_unref(op);
    pa_threaded_mainloop_unlock(m_mainloop);

    return success;
}

void PulseAudioController::enableVolumeSnap(const QString &sinkName, int stepPercent)
{
    m_snapSinkName = sinkName;
    m_snapStep = (stepPercent > 0) ? stepPercent : 5;
    LOG_INFO("Volume snap " << (sinkName.isEmpty() ? "disabled" : "enabled")
             << " sink=" << sinkName << " step=" << m_snapStep);
}

void PulseAudioController::subscribeCallback(pa_context *c,
                                             pa_subscription_event_type_t type,
                                             uint32_t idx, void *userdata)
{
    PulseAudioController *self = static_cast<PulseAudioController*>(userdata);
    if (!self || self->m_snapSinkName.isEmpty()) return;

    // Only handle sink-change events; new/remove fire on hotplug
    // and don't carry the volume we care about. PA delivers
    // facility + event-type in a packed type field — mask out the
    // top bits to recover the event.
    const auto facility = static_cast<pa_subscription_event_type_t>(
        type & PA_SUBSCRIPTION_EVENT_FACILITY_MASK);
    const auto event = static_cast<pa_subscription_event_type_t>(
        type & PA_SUBSCRIPTION_EVENT_TYPE_MASK);
    if (facility != PA_SUBSCRIPTION_EVENT_SINK) return;
    if (event != PA_SUBSCRIPTION_EVENT_CHANGE) return;

    // Get the new sink-info to read volume. Callback recurses into
    // snapSinkInfoCallback which does the actual snap. We're on the
    // PA mainloop thread so DO NOT take the lock — already held by
    // the dispatcher.
    pa_operation *op = pa_context_get_sink_info_by_index(
        c, idx, &PulseAudioController::snapSinkInfoCallback, self);
    if (op) pa_operation_unref(op);
}

void PulseAudioController::snapSinkInfoCallback(pa_context *c,
                                                const pa_sink_info *info,
                                                int eol, void *userdata)
{
    // eol > 0 marks the end of the info stream; eol < 0 is an
    // error. Either way, nothing to do.
    if (eol != 0 || !info) return;

    PulseAudioController *self = static_cast<PulseAudioController*>(userdata);
    if (!self) return;

    // Sink-name match: only snap the AirPods sink. Other sinks
    // (built-in laptop speakers, USB-DAC) keep whatever step the
    // user picked.
    const QString name = QString::fromUtf8(info->name);
    if (name != self->m_snapSinkName) return;

    // PA reports volume per-channel; for AVRCP-driven changes both
    // channels move together. Take the max as the "current" value
    // — matches what pavucontrol shows.
    pa_volume_t maxVol = 0;
    for (uint8_t ch = 0; ch < info->volume.channels; ++ch) {
        if (info->volume.values[ch] > maxVol) {
            maxVol = info->volume.values[ch];
        }
    }
    const int currentPct = static_cast<int>((maxVol * 100) / PA_VOLUME_NORM);
    const int snappedPct = snapToGrid(currentPct, self->m_snapStep);

    // Already on the grid → nothing to do. Don't even log; this
    // path fires on every keyboard volume keypress + every
    // pavucontrol drag.
    if (currentPct == snappedPct) return;

    // Echo from our own set — skip to avoid infinite loop. Even if
    // the PA daemon rounded our set value (PA stores 16-bit volume
    // internally; 5% maps cleanly but not all percentages do), the
    // delta is well under m_snapStep so the equality check via
    // snap-round absorbs that quantization.
    const int expected = self->m_expectedVolume.load(std::memory_order_relaxed);
    if (expected >= 0 && snapToGrid(currentPct, self->m_snapStep) == snapToGrid(expected, self->m_snapStep)) {
        return;
    }

    LOG_DEBUG("Volume snap: sink=" << name << " received=" << currentPct
              << "% snapping to " << snappedPct << "%");

    // Update expected before the set so the echo we're about to
    // produce gets filtered by the check above on its return trip.
    self->m_expectedVolume.store(snappedPct, std::memory_order_relaxed);

    pa_cvolume newVolume;
    pa_cvolume_set(&newVolume, info->volume.channels, (snappedPct * PA_VOLUME_NORM) / 100);
    pa_operation *op = pa_context_set_sink_volume_by_name(
        c, info->name, &newVolume, nullptr, nullptr);
    if (op) pa_operation_unref(op);
}

bool PulseAudioController::setCardProfile(const QString &cardName, const QString &profileName)
{
    if (!m_initialized) return false;

    pa_threaded_mainloop_lock(m_mainloop);
    
    auto successCallback = [](pa_context *c, int success, void *userdata) {
        pa_threaded_mainloop *mainloop = static_cast<pa_threaded_mainloop*>(userdata);
        pa_threaded_mainloop_signal(mainloop, 0);
    };
    
    pa_operation *op = pa_context_set_card_profile_by_name(m_context, 
        cardName.toUtf8().constData(), 
        profileName.toUtf8().constData(), 
        successCallback, m_mainloop);
    bool success = waitForOperation(op);
    if (op) pa_operation_unref(op);
    pa_threaded_mainloop_unlock(m_mainloop);

    return success;
}

QString PulseAudioController::getCardNameForDevice(const QString &macAddress)
{
    if (!m_initialized) return QString();

    struct CallbackData {
        QString cardName;
        QString targetMac;
        pa_threaded_mainloop *mainloop = nullptr;
    } data;
    data.targetMac = macAddress;
    data.mainloop = m_mainloop;

    auto callback = [](pa_context *c, const pa_card_info *info, int eol, void *userdata) {
        CallbackData *d = static_cast<CallbackData*>(userdata);
        // PulseAudio reports a failed query with a negative eol, which must wake the waiter too.
        if (eol != 0)
        {
            pa_threaded_mainloop_signal(d->mainloop, 0);
            return;
        }
        if (info)
        {
            QString name = QString::fromUtf8(info->name);
            if (name.startsWith("bluez") && name.contains(d->targetMac))
            {
                d->cardName = name;
                pa_threaded_mainloop_signal(d->mainloop, 0);
            }
        }
    };

    pa_threaded_mainloop_lock(m_mainloop);
    pa_operation *op = pa_context_get_card_info_list(m_context, callback, &data);
    if (op)
    {
        waitForOperation(op);
        pa_operation_unref(op);
    }
    pa_threaded_mainloop_unlock(m_mainloop);

    return data.cardName;
}

bool PulseAudioController::isProfileAvailable(const QString &cardName, const QString &profileName)
{
    if (!m_initialized) return false;

    struct CallbackData {
        bool available = false;
        QString targetCard;
        QString targetProfile;
        pa_threaded_mainloop *mainloop = nullptr;
    } data;
    data.targetCard = cardName;
    data.targetProfile = profileName;
    data.mainloop = m_mainloop;

    auto callback = [](pa_context *c, const pa_card_info *info, int eol, void *userdata) {
        CallbackData *d = static_cast<CallbackData*>(userdata);
        // PulseAudio reports a failed query with a negative eol, which must wake the waiter too.
        if (eol != 0)
        {
            pa_threaded_mainloop_signal(d->mainloop, 0);
            return;
        }
        if (info && QString::fromUtf8(info->name) == d->targetCard)
        {
            for (uint32_t i = 0; i < info->n_profiles; i++)
            {
                if (QString::fromUtf8(info->profiles[i].name) == d->targetProfile)
                {
                    d->available = true;
                    break;
                }
            }
            pa_threaded_mainloop_signal(d->mainloop, 0);
        }
    };

    pa_threaded_mainloop_lock(m_mainloop);
    pa_operation *op = pa_context_get_card_info_by_name(m_context, cardName.toUtf8().constData(), callback, &data);
    if (op)
    {
        waitForOperation(op);
        pa_operation_unref(op);
    }
    pa_threaded_mainloop_unlock(m_mainloop);

    return data.available;
}

// A stream attached to the AirPods mic is a call holding the headset profile, and a
// muted caller still holds it, so both queries run under one lock to see one server state.
CaptureState PulseAudioController::captureState(const QString &macAddress)
{
    // Both are known here rather than unanswered, and the activation path already handles them.
    if (!m_initialized || macAddress.isEmpty()) return CaptureState::Idle;

    struct SourceData {
        QVector<uint32_t> indexes;
        bool failed = false;
        QString macAddress;
        pa_threaded_mainloop *mainloop = nullptr;
    } sources;
    sources.macAddress = macAddress;
    sources.mainloop = m_mainloop;

    auto sourceCallback = [](pa_context *, const pa_source_info *info, int eol, void *userdata) {
        SourceData *d = static_cast<SourceData*>(userdata);
        if (eol != 0) {
            if (eol < 0) d->failed = true;
            pa_threaded_mainloop_signal(d->mainloop, 0);
            return;
        }
        // The card's own monitor carries the same address and runs under music, so it is excluded.
        if (!info || info->monitor_of_sink != PA_INVALID_INDEX) {
            return;
        }
        if (sourceNamesAddress(QString::fromUtf8(info->name), d->macAddress)) {
            d->indexes.append(info->index);
        }
    };

    struct StreamData {
        bool capturing = false;
        bool failed = false;
        QVector<uint32_t> indexes;
        pa_threaded_mainloop *mainloop = nullptr;
    } streams;
    streams.mainloop = m_mainloop;

    auto streamCallback = [](pa_context *, const pa_source_output_info *info, int eol, void *userdata) {
        StreamData *d = static_cast<StreamData*>(userdata);
        if (eol != 0) {
            if (eol < 0) d->failed = true;
            pa_threaded_mainloop_signal(d->mainloop, 0);
            return;
        }
        // A corked stream is an app holding the mic open without capturing, which must not pin the card.
        if (info && !info->corked && d->indexes.contains(info->source)) {
            d->capturing = true;
        }
    };

    pa_threaded_mainloop_lock(m_mainloop);
    pa_operation *sourceOp = pa_context_get_source_info_list(m_context, sourceCallback, &sources);
    const bool sourcesWaited = sourceOp != nullptr && waitForOperation(sourceOp);
    if (sourceOp) {
        pa_operation_unref(sourceOp);
    }
    bool streamsWaited = true;
    if (sourcesWaited && !sources.failed && !sources.indexes.isEmpty()) {
        streams.indexes = sources.indexes;
        pa_operation *streamOp = pa_context_get_source_output_info_list(m_context, streamCallback, &streams);
        streamsWaited = streamOp != nullptr && waitForOperation(streamOp);
        if (streamOp) {
            pa_operation_unref(streamOp);
        }
    }
    pa_threaded_mainloop_unlock(m_mainloop);

    if (!sourcesWaited || sources.failed || !streamsWaited || streams.failed) {
        LOG_WARN("PulseAudio capture query failed for " << macAddress << ", capture state unknown");
        return CaptureState::Unknown;
    }
    return streams.capturing ? CaptureState::Live : CaptureState::Idle;
}

QVector<ProfileCandidate> PulseAudioController::getCardProfiles(const QString &cardName)
{
    if (!m_initialized || cardName.isEmpty()) {
        LOG_WARN("No card to query for profiles, name=" << cardName << " initialized=" << m_initialized);
        return {};
    }

    struct CallbackData {
        QVector<ProfileCandidate> candidates;
        bool failed = false;
        QString targetCard;
        pa_threaded_mainloop *mainloop = nullptr;
    } data;
    data.targetCard = cardName;
    data.mainloop = m_mainloop;

    auto callback = [](pa_context *, const pa_card_info *info, int eol, void *userdata) {
        CallbackData *d = static_cast<CallbackData*>(userdata);
        // PulseAudio completes the operation even on an error, so record the negative eol.
        if (eol != 0) {
            if (eol < 0) d->failed = true;
            pa_threaded_mainloop_signal(d->mainloop, 0);
            return;
        }
        if (!info || QString::fromUtf8(info->name) != d->targetCard || !info->profiles2) {
            return;
        }
        // profiles2 is a null-terminated array of pointers, unlike the flat profiles list.
        for (pa_card_profile_info2 **p = info->profiles2; *p; ++p) {
            d->candidates.append(ProfileCandidate{
                QString::fromUtf8((*p)->name),
                QString::fromUtf8((*p)->description ? (*p)->description : ""),
                static_cast<int>((*p)->priority),
                (*p)->available != 0,
                (*p)->n_sinks,
                (*p)->n_sources,
            });
        }
        pa_threaded_mainloop_signal(d->mainloop, 0);
    };

    pa_threaded_mainloop_lock(m_mainloop);
    pa_operation *op = pa_context_get_card_info_by_name(
        m_context, cardName.toUtf8().constData(), callback, &data);
    const bool queried = op != nullptr && waitForOperation(op);
    if (op) {
        pa_operation_unref(op);
    }
    pa_threaded_mainloop_unlock(m_mainloop);

    if (!queried || data.failed) {
        LOG_WARN("PulseAudio card query failed for " << cardName << ", profile list is empty");
    }
    return data.candidates;
}

QString PulseAudioController::getActiveCardProfile(const QString &cardName)
{
    if (!m_initialized) return QString();

    struct CallbackData {
        QString activeProfile;
        QString targetCard;
        pa_threaded_mainloop *mainloop = nullptr;
    } data;
    data.targetCard = cardName;
    data.mainloop = m_mainloop;

    auto callback = [](pa_context *, const pa_card_info *info, int eol, void *userdata) {
        CallbackData *d = static_cast<CallbackData*>(userdata);
        // PulseAudio reports a failed query with a negative eol, which must wake the waiter too.
        if (eol != 0) {
            pa_threaded_mainloop_signal(d->mainloop, 0);
            return;
        }
        if (info && QString::fromUtf8(info->name) == d->targetCard && info->active_profile2) {
            d->activeProfile = QString::fromUtf8(info->active_profile2->name);
            pa_threaded_mainloop_signal(d->mainloop, 0);
        }
    };

    pa_threaded_mainloop_lock(m_mainloop);
    pa_operation *op = pa_context_get_card_info_by_name(
        m_context, cardName.toUtf8().constData(), callback, &data);
    if (op) {
        waitForOperation(op);
        pa_operation_unref(op);
    }
    pa_threaded_mainloop_unlock(m_mainloop);
    return data.activeProfile;
}

bool PulseAudioController::waitForOperation(pa_operation *op)
{
    if (!op) return false;

    QElapsedTimer timer; timer.start();
    constexpr qint64 kOpTimeoutMs = 3000;
    while (pa_operation_get_state(op) == PA_OPERATION_RUNNING)
    {
        if (timer.elapsed() > kOpTimeoutMs) {
            LOG_WARN("PulseAudio operation timed out after 3s");
            pa_operation_cancel(op);
            return false;
        }
        pa_threaded_mainloop_wait(m_mainloop);
    }

    return pa_operation_get_state(op) == PA_OPERATION_DONE;
}
