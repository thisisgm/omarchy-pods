#include "mediacontroller.h"
#include "capturematch.hpp"
#include "profilechoice.hpp"
#include "logger.h"
#include "eardetection.hpp"
#include "playerstatuswatcher.h"
#include "pulseaudiocontroller.h"
#include "snaptogrid.hpp"

#include <QDebug>
#include <QProcess>
#include <QThread>
#include <QRegularExpression>
#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QTimer>

// Long enough to outlast an ANC or transparency reconfigure, short enough not to strand playback.
static constexpr int bothPodsOutSettleMs = 1200;

MediaController::MediaController(QObject *parent) : QObject(parent) {
  m_pulseAudio = new PulseAudioController(this);
  if (!m_pulseAudio->initialize())
  {
    LOG_ERROR("Failed to initialize PulseAudio controller");
  }
}

void MediaController::handleEarDetection(EarDetection *earDetection)
{
  if (!earDetection)
  {
    // DeviceInfo::getEarDetection() returns a child member, so today
    // this is only null during a teardown race when handleEarDetection
    // fires after the DeviceInfo destructor has already freed the
    // EarDetection sub-object. Cheap guard avoids a SEGV on quit.
    LOG_WARN("handleEarDetection called with null EarDetection pointer");
    return;
  }
  if (earDetectionBehavior == Disabled)
  {
    LOG_DEBUG("Ear detection is disabled, ignoring status");
    return;
  }

  bool primaryInEar = earDetection->isPrimaryInEar();
  bool secondaryInEar = earDetection->isSecondaryInEar();

  LOG_DEBUG("Ear detection status: primaryInEar="
            << primaryInEar << ", secondaryInEar=" << secondaryInEar
            << ", isAirPodsActive=" << isActiveOutputDeviceAirPods());

  // An ANC or transparency change reports both pods out for a moment while the AirPods reconfigure.
  if (!primaryInEar && !secondaryInEar)
  {
    // Keep the first deadline, so a repeated both-out report cannot defer the teardown forever.
    if (m_earOutPending)
      return;
    m_earOutPending = true;
    const quint64 generation = m_earDetectionGeneration;
    QTimer::singleShot(bothPodsOutSettleMs, this, [this, earDetection, generation]() {
      // A superseded callback must not clear the window a newer report opened.
      if (generation != m_earDetectionGeneration)
        return;

      m_earOutPending = false;
      if (earDetectionBehavior == Disabled)
        return;

      if (earDetection->isPrimaryInEar() || earDetection->isSecondaryInEar())
        return;

      if (isActiveOutputDeviceAirPods() && getCurrentMediaState() == Playing)
        pause();
      removeAudioOutputDevice();
    });
    return;
  }

  // Any in-ear report supersedes a pending transient both-out report.
  ++m_earDetectionGeneration;
  m_earOutPending = false;

  // First handle playback pausing based on selected behavior
  bool shouldPause = false;
  bool shouldResume = false;

  if (earDetectionBehavior == PauseWhenOneRemoved)
  {
    shouldPause = !primaryInEar || !secondaryInEar;
    shouldResume = primaryInEar && secondaryInEar;
  }
  else if (earDetectionBehavior == PauseWhenBothRemoved)
  {
    shouldPause = !primaryInEar && !secondaryInEar;
    shouldResume = primaryInEar || secondaryInEar;
  }

  if (shouldPause && isActiveOutputDeviceAirPods())
  {
    if (getCurrentMediaState() == Playing)
    {
      LOG_DEBUG("Pausing playback for ear detection");
      pause();
    }
  }

  // Then handle device profile switching, with both pods out already returned above
  LOG_DEBUG("At least one AirPod is in ear");
  activateA2dpProfileWithRetry(connectedDeviceMacAddress);

  // Resume if conditions are met and we previously paused
  if (shouldResume && !pausedByAppServices.isEmpty() && isActiveOutputDeviceAirPods())
  {
    play();
  }
}

void MediaController::setEarDetectionBehavior(EarDetectionBehavior behavior)
{
  if (earDetectionBehavior == behavior)
  {
    LOG_DEBUG("Ear detection behavior is already set to: " << behavior);
    return;
  }

  // A pending both-out callback belongs to the policy that scheduled it, not to this one.
  ++m_earDetectionGeneration;
  m_earOutPending = false;
  earDetectionBehavior = behavior;
  LOG_INFO("Set ear detection behavior to: " << behavior);
}

void MediaController::followMediaChanges() {
  playerStatusWatcher = new PlayerStatusWatcher("", this);
  connect(playerStatusWatcher, &PlayerStatusWatcher::playbackStatusChanged,
          this, [this](const QString &status)
          {
            LOG_DEBUG("Playback status changed: " << status);
            MediaState state = mediaStateFromPlayerctlOutput(status);
            emit mediaStateChanged(state);
          });
}

bool MediaController::isActiveOutputDeviceAirPods() {
  QString defaultSink = m_pulseAudio->getDefaultSink();
  LOG_DEBUG("Default sink: " << defaultSink);
  return defaultSink.contains(connectedDeviceMacAddress);
}

void MediaController::handleConversationalAwareness(const QByteArray &data) {
    if (data.size() < 10) {
        LOG_ERROR("Invalid conversational awareness packet");
        return;
    }

    uint8_t flag = (uint8_t)data[9];

    switch (flag) {
    case 0x01:
        LOG_INFO("Conversational awareness event: voice detected");

        if (initialVolume == -1 && isActiveOutputDeviceAirPods()) {
            QString sink = m_pulseAudio->getDefaultSink();
            initialVolume = m_pulseAudio->getSinkVolume(sink);
            LOG_DEBUG("Initial volume saved: " << initialVolume << "%");
        }

        if (initialVolume != -1) {
            QString sink = m_pulseAudio->getDefaultSink();
            // Snap CA-duck target to nearest 5% so the volume label
            // matches the Quickshell keyboard-shortcut grid. Without
            // this `initialVolume * 0.20` produces off-grid values like
            // 7, 14, 19 etc. depending on user's starting volume.
            int target = snapToGrid(static_cast<int>(initialVolume * 0.20));
            m_pulseAudio->setSinkVolume(sink, target);
            LOG_INFO("Volume lowered to " << target << "%");
        }
        break;

    case 0x08:
        LOG_INFO("Conversational awareness disabled");
        initialVolume = -1;
        break;

    case 0x09:
        LOG_INFO("Conversational awareness enabled");
        break;

    default:
        LOG_INFO("Conversational awareness event: voice ended");

        if (initialVolume != -1 && isActiveOutputDeviceAirPods()) {
            QString sink = m_pulseAudio->getDefaultSink();
            m_pulseAudio->setSinkVolume(sink, initialVolume);
            LOG_INFO("Volume restored to " << initialVolume << "%");
            initialVolume = -1;
        }
        break;
    }
}


// Available when the card offers any playback profile at all.
bool MediaController::isA2dpProfileAvailable() {
  return !getPreferredA2dpProfile().isEmpty();
}

QString MediaController::getPreferredA2dpProfile() {
  if (m_deviceOutputName.isEmpty()) {
    return QString();
  }

  if (!m_cachedA2dpProfile.isEmpty() &&
      m_pulseAudio->isProfileAvailable(m_deviceOutputName, m_cachedA2dpProfile)) {
    return m_cachedA2dpProfile;
  }

  const QVector<ProfileCandidate> profiles = m_pulseAudio->getCardProfiles(m_deviceOutputName);
  m_cachedA2dpProfile = bestPlaybackProfile(profiles);
  if (!m_cachedA2dpProfile.isEmpty()) {
    // The profile name hides the codec: AAC is the bare `a2dp-sink` on this stack.
    LOG_INFO("Selected best available output profile: " << m_cachedA2dpProfile
             << " (" << profileDescription(profiles, m_cachedA2dpProfile) << ")");
  }
  return m_cachedA2dpProfile;
}

bool MediaController::restartWirePlumber() {
  LOG_INFO("Restarting WirePlumber to rediscover A2DP profiles");
  int result = QProcess::execute("systemctl", QStringList() << "--user" << "restart" << "wireplumber");
  if (result == 0) {
    LOG_INFO("WirePlumber restarted successfully");
    QThread::sleep(2);
    return true;
  } else {
    LOG_ERROR("Failed to restart WirePlumber. Do you use wireplumber?");
    return false;
  }
}

bool MediaController::activateA2dpProfile() {
  if (connectedDeviceMacAddress.isEmpty() || m_deviceOutputName.isEmpty()) {
    // Usual right after connect, PipeWire has not published the bluez5 card yet, and callers retry.
    LOG_WARN("Connected device MAC address or output name is empty, cannot activate A2DP profile");
    return false;
  }

  if (!isA2dpProfileAvailable()) {
    // A restart blocks this thread for about 2s, so a chain gets one however many attempts it takes.
    if (m_wirePlumberRestartedThisChain) {
      LOG_WARN("A2DP profile still not available, WirePlumber was already restarted for this chain");
      return false;
    }
    LOG_WARN("A2DP profile not available, attempting to restart WirePlumber");
    m_wirePlumberRestartedThisChain = true;
    if (restartWirePlumber()) {
      m_deviceOutputName = getAudioDeviceName();
      if (!isA2dpProfileAvailable()) {
        LOG_ERROR("A2DP profile still not available after WirePlumber restart");
        return false;
      }
    } else {
      LOG_ERROR("Could not restart WirePlumber, A2DP profile unavailable");
      return false;
    }
  }

  QString preferredProfile = getPreferredA2dpProfile();
  if (preferredProfile.isEmpty()) {
    LOG_ERROR("No suitable A2DP profile found");
    return false;
  }

  // Re-applying the profile already in place tears the live sink down under a playing stream.
  const QString activeProfile = m_pulseAudio->getActiveCardProfile(m_deviceOutputName);
  if (activeProfile == preferredProfile) {
    LOG_INFO("Best profile already active: " << activeProfile);
  } else {
    LOG_INFO("Activating best output profile: " << preferredProfile);
    if (!m_pulseAudio->setCardProfile(m_deviceOutputName, preferredProfile)) {
      LOG_ERROR("Failed to activate profile: " << preferredProfile);
      return false;
    }
    LOG_INFO("Profile activated: " << preferredProfile);
  }

  // Ensure the AirPods sink is selected as the default sink in PulseAudio / PipeWire
  QString sink = m_pulseAudio->getSinkForDevice(connectedDeviceMacAddress);
  if (sink.isEmpty()) {
    sink = m_pulseAudio->getDefaultSink();
  }
  if (!sink.isEmpty() && sink.contains(connectedDeviceMacAddress, Qt::CaseInsensitive)) {
    m_pulseAudio->setDefaultSink(sink);
    m_pulseAudio->enableVolumeSnap(sink, 5);
  }

  return true;
}

void MediaController::activateA2dpProfileWithRetry(const QString &macAddress) {
  if (macAddress.isEmpty()) {
    LOG_WARN("No MAC address for the connected device, cannot start A2DP profile activation");
    return;
  }

  // A fresh generation supersedes any chain still in flight, so only the newest request acts.
  const quint64 generation = ++m_a2dpRetryGeneration;
  m_wirePlumberRestartedThisChain = false;
  attemptA2dpActivation(macAddress, generation, 0);
}

void MediaController::cancelPendingA2dpActivation() {
  ++m_a2dpRetryGeneration;
}

void MediaController::attemptA2dpActivation(const QString &macAddress, quint64 generation, int attempt, int unanswered) {
  static constexpr int kMaxAttempts = 6;
  static constexpr int kDelayMs = 1500;
  static constexpr int kCaptureRecheckMs = 10000;
  static constexpr int kMaxUnansweredChecks = 6;

  // A newer request or a disconnect superseded this chain, so it must not restore a stale device.
  if (generation != m_a2dpRetryGeneration) return;

  // PipeWire publishes the bluez5 card asynchronously, so the first attempts commonly find nothing.
  setConnectedDeviceMacAddress(macAddress);

  // Switching under a live capture cuts the mic, and no other timer re-arms this ladder.
  const CaptureState capture = m_pulseAudio->captureState(connectedDeviceMacAddress);
  if (capture != CaptureState::Idle) {
    const int nextUnanswered = unansweredChecksAfter(capture, unanswered);
    if (nextUnanswered >= kMaxUnansweredChecks) {
      LOG_ERROR("Giving up on A2DP profile activation for " << macAddress
                << ", the capture query came back unknown " << nextUnanswered << " times running");
      return;
    }
    LOG_INFO("Capture " << (capture == CaptureState::Live ? "live" : "unknown")
             << " on the AirPods, re-checking in " << kCaptureRecheckMs << "ms");
    QTimer::singleShot(kCaptureRecheckMs, this, [this, macAddress, generation, attempt, nextUnanswered]() {
      attemptA2dpActivation(macAddress, generation, attempt, nextUnanswered);
    });
    return;
  }

  if (activateA2dpProfile()) {
    LOG_INFO("A2DP profile activated (attempt " << (attempt + 1) << ")");
    return;
  }

  if (attempt + 1 >= kMaxAttempts) {
    LOG_ERROR("Giving up on A2DP profile activation after " << kMaxAttempts << " attempts");
    return;
  }

  QTimer::singleShot(kDelayMs, this, [this, macAddress, generation, attempt]() {
    attemptA2dpActivation(macAddress, generation, attempt + 1);
  });
}

QString MediaController::getActiveProfile() {
  if (m_deviceOutputName.isEmpty()) return QString();
  return m_pulseAudio->getActiveCardProfile(m_deviceOutputName);
}

void MediaController::removeAudioOutputDevice() {
  // A retry still in flight would resurrect the sink right after this tears it down.
  cancelPendingA2dpActivation();

  // Disabling the snap needs no device name, so it runs before the guard that can return.
  m_pulseAudio->enableVolumeSnap(QString(), 5);

  if (connectedDeviceMacAddress.isEmpty() || m_deviceOutputName.isEmpty()) {
    LOG_WARN("Connected device MAC address or output name is empty, cannot remove audio output device");
    return;
  }

  // Ear detection fires on every packet, so without this the card is re-released every few seconds.
  if (m_pulseAudio->getActiveCardProfile(m_deviceOutputName) == QStringLiteral("off")) {
    return;
  }

  LOG_INFO("Removing AirPods as audio output device");
  if (!m_pulseAudio->setCardProfile(m_deviceOutputName, "off")) {
    LOG_ERROR("Failed to remove AirPods as audio output device");
  }
}

void MediaController::setConnectedDeviceMacAddress(const QString &macAddress) {
  connectedDeviceMacAddress = macAddress;
  m_deviceOutputName = getAudioDeviceName();
  m_cachedA2dpProfile.clear();
  LOG_INFO("Device output name set to: " << m_deviceOutputName);
}

MediaController::MediaState MediaController::mediaStateFromPlayerctlOutput(
    const QString &output) const {
  if (output == "Playing") {
    return MediaState::Playing;
  } else if (output == "Paused") {
    return MediaState::Paused;
  } else {
    return MediaState::Stopped;
  }
}

MediaController::MediaState MediaController::getCurrentMediaState() const
{
  return mediaStateFromPlayerctlOutput(PlayerStatusWatcher::getCurrentPlaybackStatus(""));
}

QStringList MediaController::getPlayingMediaPlayers()
{
  QStringList playingServices;
  QDBusConnection bus = QDBusConnection::sessionBus();

  QStringList services = bus.interface()->registeredServiceNames().value();
  for (const QString &service : services)
  {
    if (!service.startsWith("org.mpris.MediaPlayer2."))
    {
      continue;
    }

    if (PlayerStatusWatcher::playbackStatusOf(service) == "Playing")
    {
      playingServices << service;
      LOG_DEBUG("Found playing service: " << service);
    }
  }

  return playingServices;
}

void MediaController::clearPausedServices()
{
  pausedByAppServices.clear();
}

void MediaController::play()
{
  if (pausedByAppServices.isEmpty())
  {
    LOG_INFO("No services to resume");
    return;
  }

  QDBusConnection bus = QDBusConnection::sessionBus();
  int resumedCount = 0;

  for (const QString &service : pausedByAppServices)
  {
    QDBusInterface playerInterface(
        service,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
        bus);

    if (!playerInterface.isValid())
    {
      LOG_WARN("Service no longer available: " << service);
      continue;
    }

    QDBusReply<void> reply = playerInterface.call("Play");
    if (!reply.isValid())
    {
      reply = playerInterface.call("PlayPause");
    }

    if (reply.isValid())
    {
      LOG_INFO("Resumed playback for: " << service);
      resumedCount++;
    }
    else
    {
      LOG_ERROR("Failed to resume " << service << ": " << reply.error().message());
    }
  }

  if (resumedCount > 0)
  {
    LOG_INFO("Resumed " << resumedCount << " media player(s) via DBus");
    pausedByAppServices.clear();
  }
  else
  {
    LOG_ERROR("Failed to resume any media players via DBus");
  }
}

void MediaController::pause()
{
  QDBusConnection bus = QDBusConnection::sessionBus();
  QStringList services = bus.interface()->registeredServiceNames().value();

  pausedByAppServices.clear();
  int pausedCount = 0;

  for (const QString &service : services)
  {
    if (!service.startsWith("org.mpris.MediaPlayer2."))
    {
      continue;
    }

    QDBusInterface playerInterface(
        service,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
        bus);

    if (!playerInterface.isValid())
    {
      continue;
    }

    const QString playbackStatus = PlayerStatusWatcher::playbackStatusOf(service);
    LOG_DEBUG("PlaybackStatus for " << service << ": " << playbackStatus);
    if (playbackStatus != "Playing")
    {
      continue;
    }

    QDBusReply<void> reply = playerInterface.call("Pause");
    LOG_DEBUG("Pausing service: " << service);
    if (reply.isValid())
    {
      LOG_INFO("Paused playback for: " << service);
      pausedByAppServices << service;
      pausedCount++;
    }
    else
    {
      LOG_ERROR("Failed to pause " << service << ": " << reply.error().message());
    }
  }

  if (pausedCount > 0)
  {
    LOG_INFO("Paused " << pausedCount << " media player(s) via DBus");
  }
  else
  {
    LOG_INFO("No playing media players found to pause");
  }
}

MediaController::~MediaController() {
}

QString MediaController::getAudioDeviceName()
{
  if (connectedDeviceMacAddress.isEmpty()) { return QString(); }

  QString cardName = m_pulseAudio->getCardNameForDevice(connectedDeviceMacAddress);
  if (cardName.isEmpty()) {
    LOG_ERROR("No matching Bluetooth card found for MAC address: " << connectedDeviceMacAddress);
  }
  return cardName;
}
