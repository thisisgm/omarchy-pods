#pragma once

#include <QByteArrayView>
#include <QObject>
#include <QString>

#include "agc.hpp"
#include "hiresframe.hpp"

struct AVCodecContext;
struct AVFrame;
struct AVPacket;

// Decodes the AirPods AAC-ELD uplink and feeds it to a PipeWire source, so the AirPods
// microphone is available while playback stays on A2DP.
class HiResMic : public QObject
{
    Q_OBJECT

public:
    explicit HiResMic(QObject *parent = nullptr);
    ~HiResMic() override;

    // Opens the decoder and publishes the source, adopting one an earlier run left loaded.
    // False leaves nothing behind.
    bool start(const QString &description);
    // Closes the decoder and the writer but keeps the source: Discord's voice engine aborts
    // when any microphone appears or disappears while it records, so the source outlives
    // disconnects and restarts and is unloaded only by remove().
    void stop();
    void remove();
    bool isRunning() const { return m_module > 0; }
    static constexpr const char *sourceName() { return "AirPodsHiRes"; }

    // 64 kHz mono s16 from another path, written as the uplink's own frames are.
    void writePcm(QByteArrayView pcm);

    // One AAC-ELD access unit from an 0x58 audio SDU.
    void decode(QByteArrayView accessUnit);

    quint64 framesDecoded() const { return m_frames; }
    quint64 decodeErrors() const { return m_errors; }

private:
    bool openDecoder();
    void closeDecoder();
    bool publishSource(const QString &description);
    bool adoptSource(const QString &label);
    void unloadSource();
    void closeWriter();
    QString fifoPath() const;
    void writeFifo(QByteArrayView pcm);

    AVCodecContext *m_ctx = nullptr;
    AVFrame *m_frame = nullptr;
    AVPacket *m_pkt = nullptr;
    int m_fifo = -1;
    unsigned m_module = 0;
    OpenPods::HiRes::Preamp m_preamp;
    OpenPods::HiRes::SampleWriter m_writer;
    quint64 m_frames = 0;
    quint64 m_errors = 0;
};
