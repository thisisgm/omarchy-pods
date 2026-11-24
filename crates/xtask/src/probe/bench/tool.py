"""The steps of the bench that call a C library: cut a reference, and pass it through libsbc or libfdk-aac with the settings given."""

import ctypes
import importlib.metadata
import platform
import sys

import numpy
import soundfile
from scipy.signal import resample_poly

CHANNELS = 2
SAMPLE_BYTES = 2
FULL_SCALE = 32768.0
# No frame comes near it: the peak rate the encoder is given makes frames of about 1 KiB.
MAX_FRAME_BYTES = 8192
AACENC_TRANSMUX = 0x0300
AAC_PCM_MIN_OUTPUT_CHANNELS = 0x0011
AAC_PCM_MAX_OUTPUT_CHANNELS = 0x0012
IN_AUDIO_DATA = 0
OUT_BITSTREAM_DATA = 3
# What aacEncEncode takes as the sample count once the input has ended.
END_OF_INPUT = -1
# What aacEncEncode returns once it has given every frame of an input that has ended.
AACENC_ENCODE_EOF = 0x0080


def fail(message):
    sys.exit(f"bench tool: {message}")


def read_pcm(path):
    samples, rate = soundfile.read(path, dtype="int16", always_2d=True)
    if samples.shape[1] != CHANNELS:
        fail(f"{path} holds {samples.shape[1]} channels, not {CHANNELS}")
    return samples.astype("<i2").tobytes(), rate


def write_pcm(path, interleaved, rate):
    frames = numpy.frombuffer(interleaved, dtype="<i2").reshape(-1, CHANNELS)
    soundfile.write(path, frames, rate, subtype="PCM_16")


# Sample input: `frequency=3 bitpool=39`, or `0x0100=129` where the library names a setting by number.
def settings_of(pairs):
    return [(name, int(value)) for name, value in (pair.split("=") for pair in pairs)]


def versions():
    print(f"python\t{platform.python_version()}")
    for package in sorted(importlib.metadata.distributions(), key=lambda package: package.name):
        print(f"{package.name}\t{package.version}")


def excerpt(source, start_s, seconds, rate, out):
    info = soundfile.info(source)
    if info.channels != CHANNELS:
        fail(f"{source} holds {info.channels} channels, not {CHANNELS}")
    first, count = int(start_s) * info.samplerate, int(seconds) * info.samplerate
    samples, _ = soundfile.read(source, start=first, frames=count, dtype="float64", always_2d=True)
    if len(samples) != count:
        fail(f"{source} ends before {int(start_s) + int(seconds)} s")
    resampled = resample_poly(samples, int(rate), info.samplerate, axis=0)
    pcm = numpy.clip(numpy.rint(resampled * FULL_SCALE), -FULL_SCALE, FULL_SCALE - 1)
    soundfile.write(out, pcm.astype("<i2"), int(rate), subtype="PCM_16")


class Sbc(ctypes.Structure):
    _fields_ = [
        ("flags", ctypes.c_ulong),
        ("frequency", ctypes.c_uint8),
        ("blocks", ctypes.c_uint8),
        ("subbands", ctypes.c_uint8),
        ("mode", ctypes.c_uint8),
        ("allocation", ctypes.c_uint8),
        ("bitpool", ctypes.c_uint8),
        ("endian", ctypes.c_uint8),
        ("priv", ctypes.c_void_p),
        ("priv_alloc_base", ctypes.c_void_p),
    ]


def sbc_codec(lib, settings):
    codec = Sbc()
    # SAFETY: codec is a whole sbc_t laid out as sbc.h declares it, alive for the call; sbc_init only fills it.
    if lib.sbc_init(ctypes.byref(codec), 0) != 0:
        fail("sbc_init failed")
    for name, value in settings:
        setattr(codec, name, value)
    return codec


def sbc(library, source, out, *pairs):
    lib = ctypes.CDLL(library)
    block_call = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p]
    for call in (lib.sbc_encode, lib.sbc_decode):
        call.restype, call.argtypes = ctypes.c_ssize_t, block_call
    lib.sbc_get_codesize.restype = ctypes.c_size_t
    settings = settings_of(pairs)
    encoder, decoder = sbc_codec(lib, settings), sbc_codec(lib, settings)
    pcm, rate = read_pcm(source)
    # SAFETY: encoder was set up by sbc_init above and lives until the function returns.
    codesize = lib.sbc_get_codesize(ctypes.byref(encoder))
    frame, block = ctypes.create_string_buffer(MAX_FRAME_BYTES), ctypes.create_string_buffer(codesize)
    decoded = bytearray()
    for at in range(0, len(pcm) - codesize + 1, codesize):
        written = ctypes.c_ssize_t(0)
        # SAFETY: the input slice is exactly codesize bytes and the output buffer is MAX_FRAME_BYTES, the length passed.
        taken = lib.sbc_encode(ctypes.byref(encoder), pcm[at : at + codesize], codesize, frame, MAX_FRAME_BYTES, ctypes.byref(written))
        if taken != codesize or written.value <= 0:
            fail(f"sbc_encode took {taken} bytes and wrote {written.value}")
        size = ctypes.c_size_t(0)
        # SAFETY: written.value bytes of frame are valid and block holds codesize bytes, the length passed.
        read = lib.sbc_decode(ctypes.byref(decoder), frame, written.value, block, codesize, ctypes.byref(size))
        if read != written.value or size.value != codesize:
            fail(f"sbc_decode took {read} bytes and wrote {size.value}")
        decoded += block.raw
    write_pcm(out, decoded, rate)


class BufDesc(ctypes.Structure):
    _fields_ = [
        ("numBufs", ctypes.c_int),
        ("bufs", ctypes.POINTER(ctypes.c_void_p)),
        ("bufferIdentifiers", ctypes.POINTER(ctypes.c_int)),
        ("bufSizes", ctypes.POINTER(ctypes.c_int)),
        ("bufElSizes", ctypes.POINTER(ctypes.c_int)),
    ]


class InArgs(ctypes.Structure):
    _fields_ = [("numInSamples", ctypes.c_int), ("numAncBytes", ctypes.c_int)]


class OutArgs(ctypes.Structure):
    _fields_ = [("numOutBytes", ctypes.c_int), ("numInSamples", ctypes.c_int), ("numAncBytes", ctypes.c_int), ("bitResState", ctypes.c_int)]


class EncInfo(ctypes.Structure):
    _fields_ = [
        ("maxOutBufBytes", ctypes.c_uint),
        ("maxAncBytes", ctypes.c_uint),
        ("inBufFillLevel", ctypes.c_uint),
        ("inputChannels", ctypes.c_uint),
        ("frameLength", ctypes.c_uint),
        ("nDelay", ctypes.c_uint),
        ("nDelayCore", ctypes.c_uint),
        ("confBuf", ctypes.c_ubyte * 64),
        ("confSize", ctypes.c_uint),
    ]


# The parts are returned too: the description only points at them, so they must live as long as it does.
def buffer_of(memory, size, identifier):
    parts = (ctypes.c_void_p(ctypes.addressof(memory)), ctypes.c_int(identifier), ctypes.c_int(size), ctypes.c_int(SAMPLE_BYTES))
    return BufDesc(1, *(ctypes.pointer(part) for part in parts)), parts


def aac_encoder(lib, settings):
    encoder = ctypes.c_void_p()
    # SAFETY: encoder is a pointer slot the library fills; it lives as long as this process.
    if lib.aacEncOpen(ctypes.byref(encoder), 0, CHANNELS) != 0:
        fail("aacEncOpen failed")
    for parameter, value in settings:
        # SAFETY: encoder is the handle aacEncOpen returned, checked non failing above.
        status = lib.aacEncoder_SetParam(encoder, parameter, value)
        if status != 0:
            fail(f"aacEncoder_SetParam({parameter:#06x}, {value}) returned {status:#x}")
    info = EncInfo()
    # SAFETY: null buffers are the documented way to apply settings, and info is a whole AACENC_InfoStruct.
    if lib.aacEncEncode(encoder, None, None, None, None) != 0 or lib.aacEncInfo(encoder, ctypes.byref(info)) != 0:
        fail("the encoder refused its settings")
    return encoder, info.frameLength


def aac_decoder(lib, transport):
    # SAFETY: plain integer arguments; the returned handle is checked for null below.
    decoder = lib.aacDecoder_Open(transport, 1)
    if not decoder:
        fail("aacDecoder_Open failed")
    for parameter in (AAC_PCM_MIN_OUTPUT_CHANNELS, AAC_PCM_MAX_OUTPUT_CHANNELS):
        # SAFETY: decoder is the non null handle aacDecoder_Open returned.
        if lib.aacDecoder_SetParam(decoder, parameter, CHANNELS) != 0:
            fail(f"aacDecoder_SetParam({parameter:#06x}) failed")
    return decoder


def aac(library, source, out, *pairs):
    lib = ctypes.CDLL(library)
    lib.aacDecoder_Open.restype = ctypes.c_void_p
    lib.aacDecoder_Fill.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(ctypes.c_uint), ctypes.POINTER(ctypes.c_uint)]
    lib.aacDecoder_DecodeFrame.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int, ctypes.c_uint]
    lib.aacDecoder_SetParam.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int]
    lib.aacEncoder_SetParam.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_uint]
    settings = [(int(name, 0), value) for name, value in settings_of(pairs)]
    encoder, frame_length = aac_encoder(lib, settings)
    decoder = aac_decoder(lib, dict(settings)[AACENC_TRANSMUX])
    pcm, rate = read_pcm(source)
    codesize = frame_length * CHANNELS * SAMPLE_BYTES
    whole = len(pcm) - len(pcm) % codesize
    frame = ctypes.create_string_buffer(MAX_FRAME_BYTES)
    block = (ctypes.c_int16 * (frame_length * CHANNELS))()
    decoded = bytearray()

    # One block in, or the end of the input, and what the decoder makes of the frame that comes out; false once the encoder is empty.
    def through(chunk, count):
        memory = ctypes.create_string_buffer(chunk, codesize)
        fed, _fed_parts = buffer_of(memory, codesize, IN_AUDIO_DATA)
        made, _made_parts = buffer_of(frame, MAX_FRAME_BYTES, OUT_BITSTREAM_DATA)
        in_args, out_args = InArgs(count, 0), OutArgs()
        # SAFETY: memory holds codesize bytes and frame MAX_FRAME_BYTES, the sizes the buffer descriptors state.
        status = lib.aacEncEncode(encoder, ctypes.byref(fed), ctypes.byref(made), ctypes.byref(in_args), ctypes.byref(out_args))
        if status == AACENC_ENCODE_EOF and count == END_OF_INPUT:
            return False
        if status != 0:
            fail(f"aacEncEncode returned {status:#x}")
        if out_args.numOutBytes == 0:
            return count != END_OF_INPUT
        start, size = ctypes.c_void_p(ctypes.addressof(frame)), ctypes.c_uint(out_args.numOutBytes)
        left = ctypes.c_uint(out_args.numOutBytes)
        # SAFETY: start points at frame and size is numOutBytes, which the encoder wrote inside MAX_FRAME_BYTES.
        if lib.aacDecoder_Fill(decoder, ctypes.byref(start), ctypes.byref(size), ctypes.byref(left)) != 0 or left.value != 0:
            fail("aacDecoder_Fill did not take the frame")
        # SAFETY: block holds len(block) samples, the count passed, which is one frame of frame_length per channel.
        status = lib.aacDecoder_DecodeFrame(decoder, block, len(block), 0)
        if status != 0:
            fail(f"aacDecoder_DecodeFrame returned {status:#x}")
        decoded.extend(bytes(block))
        return True

    for at in range(0, whole, codesize):
        through(pcm[at : at + codesize], frame_length * CHANNELS)
    while through(b"", END_OF_INPUT):
        pass
    write_pcm(out, decoded, rate)


if __name__ == "__main__":
    {"versions": versions, "excerpt": excerpt, "sbc": sbc, "aac": aac}[sys.argv[1]](*sys.argv[2:])
