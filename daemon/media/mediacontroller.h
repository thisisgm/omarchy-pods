#ifndef MEDIACONTROLLER_H
#define MEDIACONTROLLER_H

#include <QObject>

#include "capturematch.hpp"
#include "pulseaudiocontroller.h"

class QProcess;
class EarDetection;
class PlayerStatusWatcher;
class QDBusInterface;

class MediaController : public QObject
{
  Q_OBJECT
public:
  enum MediaState
  {
    Playing,
    Paused,
    Stopped
  };
  Q_ENUM(MediaState)
  enum EarDetectionBehavior
  {
    PauseWhenOneRemoved,
    PauseWhenBothRemoved,
    Disabled
  };
  Q_ENUM(EarDetectionBehavior)

  explicit MediaController(QObject *parent = nullptr);
  ~MediaController();

  void handleEarDetection(EarDetection*);
  void followMediaChanges();
  bool isActiveOutputDeviceAirPods();
  void handleConversationalAwareness(const QByteArray &data);
  void activateA2dpProfileWithRetry(const QString &macAddress);
  void cancelPendingA2dpActivation();
  void removeAudioOutputDevice();
  void setConnectedDeviceMacAddress(const QString &macAddress);
  bool isA2dpProfileAvailable();
  QString getPreferredA2dpProfile();
  QString getActiveProfile();
  bool restartWirePlumber();

  void setEarDetectionBehavior(EarDetectionBehavior behavior);
  // Whether anything is recording from a source, named exactly. The hi-res mic monitor asks.
  CaptureState captureStateForSource(const QString &sourceName);
  PulseAudioController *pulseAudio() const { return m_pulseAudio; }
  // After a hi-res mic STOP the AirPods play from one bud until the A2DP transport restarts.
  // A sink suspend and resume restarts it without removing the sink; a profile cycle is the fallback.
  // blocking waits out the resume instead of scheduling it, for the quit path with no event loop.
  void resetA2dpTransport(const QString &macAddress, bool blocking = false);
  inline EarDetectionBehavior getEarDetectionBehavior() const { return earDetectionBehavior; }

  void play();
  void pause();
  MediaState getCurrentMediaState() const;

Q_SIGNALS:
  void mediaStateChanged(MediaState state);

private:
  MediaState mediaStateFromPlayerctlOutput(const QString &output) const;
  QString getAudioDeviceName();
  QStringList getPlayingMediaPlayers();
  // Only the retry chain calls this, so the one-restart flag below cannot be read outside a chain.
  bool activateA2dpProfile();
  void resumeSink(const QString &sink, bool blocking, int attempt = 0);
  void attemptA2dpActivation(const QString &macAddress, quint64 generation, int attempt, int unanswered = 0);

  QStringList pausedByAppServices;
  int initialVolume = -1;
  QString connectedDeviceMacAddress;
  EarDetectionBehavior earDetectionBehavior = PauseWhenOneRemoved;
  QString m_deviceOutputName;
  PlayerStatusWatcher *playerStatusWatcher = nullptr;
  PulseAudioController *m_pulseAudio = nullptr;
  QString m_cachedA2dpProfile;
  quint64 m_earDetectionGeneration = 0;
  bool m_earOutPending = false;
  // A queued retry compares its captured generation against this, so a superseded chain stops.
  quint64 m_a2dpRetryGeneration = 0;
  bool m_wirePlumberRestartedThisChain = false;
};

#endif // MEDIACONTROLLER_H
