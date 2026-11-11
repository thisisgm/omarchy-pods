#pragma once

#include <QByteArrayView>
#include <QObject>
#include <QString>

#include "agc.hpp"

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

    // Opens the decoder and publishes the source. False leaves nothing behind.
    bool start();
    void stop();
    bool isRunning() const { return m_module > 0; }

    // One AAC-ELD access unit from an 0x58 audio SDU.
    void decode(QByteArrayView accessUnit);

    quint64 framesDecoded() const { return m_frames; }
    quint64 decodeErrors() const { return m_errors; }

private:
    bool openDecoder();
    void closeDecoder();
    bool publishSource();
    void unpublishSource();
    QString fifoPath() const;

    AVCodecContext *m_ctx = nullptr;
    AVFrame *m_frame = nullptr;
    AVPacket *m_pkt = nullptr;
    int m_fifo = -1;
    unsigned m_module = 0;
    OpenPods::HiRes::Preamp m_preamp;
    quint64 m_frames = 0;
    quint64 m_errors = 0;
};
