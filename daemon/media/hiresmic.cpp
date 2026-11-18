#include "hiresmic.h"

#include "agc.hpp"
#include "eldpcm.hpp"
#include "../logger.h"

#include <QDir>
#include <QFile>
#include <QProcess>
#include <QStandardPaths>

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/mem.h>
}

namespace {
// AudioSpecificConfig for the uplink: AOT 39 (AAC-ELD), 480-sample frames.
constexpr quint8 kEldAsc[] = {0xF8, 0xE6, 0x30, 0x00};
// The stream decodes to 64 kHz, but its coding rate is 48 kHz and the decoder needs that one.
constexpr int kEldCodingRate = 48000;
constexpr const char *kSourceName = "AirPodsHiRes";
}

HiResMic::HiResMic(QObject *parent) : QObject(parent) {}

HiResMic::~HiResMic()
{
    stop();
}

QString HiResMic::fifoPath() const
{
    const QString runtime = QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation);
    return runtime + "/librepods-hires.fifo";
}

bool HiResMic::openDecoder()
{
    av_log_set_level(AV_LOG_FATAL);
    const AVCodec *codec = avcodec_find_decoder(AV_CODEC_ID_AAC);
    if (!codec) {
        LOG_ERROR("No AAC decoder in this libavcodec, cannot decode the hi-res uplink");
        return false;
    }
    m_ctx = avcodec_alloc_context3(codec);
    m_frame = av_frame_alloc();
    m_pkt = av_packet_alloc();
    if (!m_ctx || !m_frame || !m_pkt) {
        closeDecoder();
        return false;
    }
    m_ctx->extradata = static_cast<uint8_t *>(av_mallocz(sizeof(kEldAsc) + AV_INPUT_BUFFER_PADDING_SIZE));
    if (!m_ctx->extradata) {
        closeDecoder();
        return false;
    }
    memcpy(m_ctx->extradata, kEldAsc, sizeof(kEldAsc));
    m_ctx->extradata_size = sizeof(kEldAsc);
    m_ctx->sample_rate = kEldCodingRate;
    if (avcodec_open2(m_ctx, codec, nullptr) < 0) {
        LOG_ERROR("Could not open the AAC-ELD decoder");
        closeDecoder();
        return false;
    }
    return true;
}

void HiResMic::closeDecoder()
{
    if (m_pkt) av_packet_free(&m_pkt);
    if (m_frame) av_frame_free(&m_frame);
    if (m_ctx) avcodec_free_context(&m_ctx);
}

namespace {
// Every module-pipe-source this daemon loaded, by index, from "pactl list modules short".
QList<QPair<unsigned, QString>> ourModules()
{
    QProcess pactl;
    pactl.start("pactl", {"list", "modules", "short"});
    QList<QPair<unsigned, QString>> modules;
    if (!pactl.waitForFinished(5000) || pactl.exitCode() != 0) {
        return modules;
    }
    const QString marker = QStringLiteral("source_name=%1 ").arg(kSourceName);
    for (const QString &line : QString::fromUtf8(pactl.readAllStandardOutput()).split('\n')) {
        const QStringList fields = line.split('\t');
        if (fields.size() >= 3 && fields[1] == QStringLiteral("module-pipe-source") && fields[2].contains(marker)) {
            modules.append({fields[0].toUInt(), fields[2]});
        }
    }
    return modules;
}

bool sourceExists()
{
    QProcess pactl;
    pactl.start("pactl", {"list", "sources", "short"});
    if (!pactl.waitForFinished(5000) || pactl.exitCode() != 0) {
        return false;
    }
    for (const QString &line : QString::fromUtf8(pactl.readAllStandardOutput()).split('\n')) {
        const QStringList fields = line.split('\t');
        if (fields.size() >= 2 && fields[1] == QLatin1String(kSourceName)) {
            return true;
        }
    }
    return false;
}

bool isFifo(const QString &path)
{
    struct stat st {};
    return stat(path.toLocal8Bit().constData(), &st) == 0 && S_ISFIFO(st.st_mode);
}
}

bool HiResMic::adoptSource()
{
    const QString fifo = fifoPath();
    const auto modules = ourModules();
    unsigned keep = 0;
    if (isFifo(fifo) && sourceExists()) {
        for (const auto &module : modules) {
            if (module.second.contains(QStringLiteral("file=%1 ").arg(fifo))) {
                keep = module.first;
                break;
            }
        }
    }
    for (const auto &module : modules) {
        if (module.first != keep) {
            LOG_WARN("Unloading stale hi-res module " << module.first);
            QProcess::execute("pactl", {"unload-module", QString::number(module.first)});
        }
    }
    if (keep == 0) {
        return false;
    }
    m_fifo = open(fifo.toLocal8Bit().constData(), O_RDWR | O_NONBLOCK | O_CLOEXEC);
    if (m_fifo < 0) {
        QProcess::execute("pactl", {"unload-module", QString::number(keep)});
        return false;
    }
    m_module = keep;
    LOG_INFO("Reusing the hi-res mic source left by an earlier run, module " << m_module);
    return true;
}

bool HiResMic::publishSource(const QString &description)
{
    if (adoptSource()) {
        return true;
    }
    const QString fifo = fifoPath();
    QFile::remove(fifo);
    if (mkfifo(fifo.toLocal8Bit().constData(), 0600) != 0) {
        LOG_ERROR("Could not create the hi-res fifo at " << fifo);
        return false;
    }
    // Opened read-write so the writer never blocks waiting for a reader to appear.
    m_fifo = open(fifo.toLocal8Bit().constData(), O_RDWR | O_NONBLOCK | O_CLOEXEC);
    if (m_fifo < 0) {
        LOG_ERROR("Could not open the hi-res fifo at " << fifo);
        QFile::remove(fifo);
        return false;
    }

    QString label = description;
    label.remove('"').remove('\'').remove('\\');
    // node.virtual=false gives the source the HARDWARE flag, so desktop volume applets list it.
    QProcess pactl;
    pactl.start("pactl", {"load-module", "module-pipe-source",
                          QStringLiteral("source_name=%1").arg(kSourceName),
                          QStringLiteral("file=%1").arg(fifo),
                          "format=s16le",
                          QStringLiteral("rate=%1").arg(OpenPods::HiRes::kEldSampleRate),
                          QStringLiteral("channels=%1").arg(OpenPods::HiRes::kEldChannels),
                          "channel_map=mono",
                          QStringLiteral("source_properties='device.description=\"%1\" node.driver=false "
                                         "priority.driver=0 node.virtual=false'").arg(label)});
    if (!pactl.waitForFinished(5000) || pactl.exitCode() != 0) {
        LOG_ERROR("pactl refused module-pipe-source: " << pactl.readAllStandardError().trimmed());
        ::close(m_fifo);
        m_fifo = -1;
        QFile::remove(fifo);
        return false;
    }
    m_module = pactl.readAllStandardOutput().trimmed().toUInt();
    LOG_INFO("Hi-res mic source '" << kSourceName << "' (" << label << ") ready, module " << m_module);
    return m_module > 0;
}

void HiResMic::closeWriter()
{
    if (m_fifo >= 0) {
        ::close(m_fifo);
        m_fifo = -1;
    }
}

void HiResMic::unloadSource()
{
    if (m_module > 0) {
        QProcess::execute("pactl", {"unload-module", QString::number(m_module)});
        m_module = 0;
    }
    closeWriter();
    QFile::remove(fifoPath());
}

bool HiResMic::start(const QString &description)
{
    if (isRunning()) {
        return true;
    }
    if (!openDecoder()) {
        return false;
    }
    if (!publishSource(description)) {
        closeDecoder();
        return false;
    }
    m_frames = 0;
    m_errors = 0;
    return true;
}

void HiResMic::stop()
{
    closeWriter();
    m_module = 0;
    closeDecoder();
}

void HiResMic::remove()
{
    if (m_module == 0) {
        adoptSource();
    }
    unloadSource();
    closeDecoder();
}

void HiResMic::writePcm(QByteArrayView pcm)
{
    if (m_fifo < 0 || pcm.isEmpty()) {
        return;
    }
    // A full pipe drops, as in decode().
    if (::write(m_fifo, pcm.data(), pcm.size()) < 0 && errno != EAGAIN) {
        ++m_errors;
    }
}

void HiResMic::decode(QByteArrayView accessUnit)
{
    if (!m_ctx || m_fifo < 0 || accessUnit.isEmpty()) {
        return;
    }
    // av_packet_from_data wants a padded, writable buffer, so the unit is copied into one.
    QByteArray padded(accessUnit.size() + AV_INPUT_BUFFER_PADDING_SIZE, '\0');
    memcpy(padded.data(), accessUnit.data(), accessUnit.size());
    m_pkt->data = reinterpret_cast<uint8_t *>(padded.data());
    m_pkt->size = static_cast<int>(accessUnit.size());

    if (avcodec_send_packet(m_ctx, m_pkt) < 0) {
        ++m_errors;
        return;
    }
    while (avcodec_receive_frame(m_ctx, m_frame) == 0) {
        const int samples = m_frame->nb_samples;
        QByteArray pcm(samples * static_cast<int>(sizeof(qint16)), '\0');
        auto *out = reinterpret_cast<qint16 *>(pcm.data());
        const auto *in = reinterpret_cast<const float *>(m_frame->extended_data[0]);
        for (int i = 0; i < samples; ++i) {
            out[i] = OpenPods::HiRes::floatToS16(in[i]);
        }
        m_preamp.process(out, samples);
        // A reader that stopped draining must not stall the control link, so a full pipe drops.
        const ssize_t written = ::write(m_fifo, pcm.constData(), pcm.size());
        if (written < 0 && errno != EAGAIN) {
            ++m_errors;
        }
        ++m_frames;
    }
}
