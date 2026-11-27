//! Checked readers and additional tag writers used by audio server fixtures.

use std::ffi::{CStr, CString};
use std::io;

use super::{
    ChannelVolume, DeviceLookup, INDEX_NONE, MAX_CHANNELS, Props, ProtocolError, Result, TagWriter,
    Volume, tag,
};

#[derive(Debug)]
pub(crate) struct Reader<'a>(pub(super) &'a [u8]);

impl<'a> Reader<'a> {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn finish(self) -> Result<()> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::Invalid(
                "trailing bytes in PulseAudio message",
            ))
        }
    }

    // Sample: take(2) consumes 01 02 from 01 02 03.
    pub(super) fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let (bytes, rest) = self
            .0
            .split_at_checked(count)
            .ok_or(ProtocolError::Invalid("cut PulseAudio field"))?;
        self.0 = rest;
        Ok(bytes)
    }

    // Sample: 00 00 00 07 reads as 7.
    fn raw_u32(&mut self) -> Result<u32> {
        let (bytes, rest) = self
            .0
            .split_first_chunk()
            .ok_or(ProtocolError::Invalid("cut PulseAudio integer"))?;
        self.0 = rest;
        Ok(u32::from_be_bytes(*bytes))
    }

    // Sample: L is the u32 tag.
    pub(super) fn tag(&mut self, expected: u8) -> Result<()> {
        if self.take(1)? == [expected] {
            Ok(())
        } else {
            Err(ProtocolError::Invalid("unexpected PulseAudio tag"))
        }
    }

    // Sample: L 00000007 reads as 7.
    pub(super) fn u32(&mut self) -> Result<u32> {
        self.tag(tag::U32)?;
        self.raw_u32()
    }

    // Sample: B 01 reads as 1.
    pub(super) fn u8(&mut self) -> Result<u8> {
        self.tag(tag::U8)?;
        self.byte()
    }

    // Sample: ff reads as 255.
    fn byte(&mut self) -> Result<u8> {
        self.0
            .split_first()
            .map(|(&byte, rest)| {
                self.0 = rest;
                byte
            })
            .ok_or(ProtocolError::Invalid("cut PulseAudio byte"))
    }

    // Sample: L ffffffff represents an absent index.
    pub(super) fn index(&mut self) -> Result<Option<u32>> {
        let index = self.u32()?;
        Ok((index != INDEX_NONE).then_some(index))
    }

    // Sample: 1 reads as true.
    pub(super) fn boolean(&mut self) -> Result<bool> {
        match self.byte()? {
            tag::TRUE => Ok(true),
            tag::FALSE => Ok(false),
            _ => Err(ProtocolError::Invalid("invalid PulseAudio boolean")),
        }
    }

    // Sample: t sink\0 reads as a name, N as null.
    pub(super) fn string(&mut self) -> Result<Option<CString>> {
        match self.byte()? {
            tag::STRING_NULL => Ok(None),
            tag::STRING => {
                let text = CStr::from_bytes_until_nul(self.0)
                    .map_err(|_error| ProtocolError::Invalid("unterminated PulseAudio string"))?;
                self.take(text.to_bytes_with_nul().len())?;
                Ok(Some(text.to_owned()))
            }
            _ => Err(ProtocolError::Invalid("invalid PulseAudio string tag")),
        }
    }

    // Sample: t sink\0 reads as a required name.
    pub(super) fn name(&mut self) -> Result<CString> {
        self.string()?
            .ok_or(ProtocolError::Invalid("missing PulseAudio name"))
    }

    // Sample: L ffffffff t sink\0 selects a sink by name.
    pub(super) fn device(&mut self) -> Result<DeviceLookup> {
        Ok(DeviceLookup {
            index: self.index()?,
            name: self.string()?,
        })
    }

    // Sample: x 00000002 ff 00 reads as two bytes.
    pub(super) fn arbitrary(&mut self) -> Result<&'a [u8]> {
        self.tag(tag::ARBITRARY)?;
        let length = self.raw_u32()?;
        self.take(
            usize::try_from(length)
                .map_err(|_error| ProtocolError::Invalid("invalid PulseAudio length"))?,
        )
    }

    // Sample: P t key\0 L 00000002 x 00000002 v\0 N reads as one property.
    pub(super) fn props(&mut self) -> Result<Props> {
        self.tag(tag::PROPLIST)?;
        let mut props = Props::new();
        while let Some(key) = self.string()? {
            let length = self.u32()?;
            let value = self.arbitrary()?;
            if usize::try_from(length) != Ok(value.len()) {
                return Err(ProtocolError::Invalid(
                    "inconsistent PulseAudio property size",
                ));
            }
            props.set_bytes(key, value);
        }
        Ok(props)
    }

    // Sample: v 02 00010000 00008000 gives two channel volumes.
    pub(super) fn channels(&mut self) -> Result<ChannelVolume> {
        self.tag(tag::CVOLUME)?;
        let count = self.byte()?;
        if !(1..=MAX_CHANNELS).contains(&usize::from(count)) {
            return Err(ProtocolError::Invalid("invalid PulseAudio channel count"));
        }
        let mut volume = ChannelVolume::empty();
        for _ in 0..count {
            volume.push(Volume::from_u32_clamped(self.raw_u32()?));
        }
        Ok(volume)
    }

    // Sample: a 03 02 0000bb80 m 02 01 02 is stereo S16LE at 48 kHz.
    pub(super) fn sample_and_map(&mut self) -> Result<u32> {
        let rate = self.sample_rate()?;
        self.skip_map()?;
        Ok(rate)
    }

    // Sample: a 03 02 0000bb80 is stereo S16LE at 48 kHz.
    pub(super) fn sample_rate(&mut self) -> Result<u32> {
        self.tag(tag::SAMPLE_SPEC)?;
        self.byte()?;
        self.byte()?;
        self.raw_u32()
    }

    // Sample: m 02 01 02 maps the front-left and front-right channels.
    pub(super) fn skip_map(&mut self) -> Result<()> {
        self.tag(tag::CHANNEL_MAP)?;
        let count = self.byte()?;
        self.take(usize::from(count))?;
        Ok(())
    }

    // Sample: U 0000000000000000 gives a zero latency.
    pub(super) fn skip_latency(&mut self) -> Result<()> {
        self.tag(tag::USEC)?;
        self.take(size_of::<u64>())?;
        Ok(())
    }

    // Sample: f B 01 P N is PCM with no format properties.
    pub(super) fn format(&mut self) -> Result<Props> {
        self.tag(tag::FORMAT_INFO)?;
        self.u8()?;
        self.props()
    }
}

const S16LE: u8 = 3;
const STEREO: u8 = 2;
const STEREO_POSITIONS: [u8; 2] = [1, 2];
const PCM: u8 = 1;

pub(super) trait FixtureTags {
    fn u8(&mut self, value: u8);
    fn latency(&mut self);
    fn sample_and_map(&mut self, rate: u32);
    fn sample_spec(&mut self, rate: u32);
    fn channel_map(&mut self);
    fn volume(&mut self, volume: Volume);
    fn format(&mut self, props: &Props) -> io::Result<()>;
}

impl FixtureTags for TagWriter<'_> {
    fn u8(&mut self, value: u8) {
        self.0.extend_from_slice(&[tag::U8, value]);
    }

    fn latency(&mut self) {
        self.0.push(tag::USEC);
        self.0.extend_from_slice(&0_u64.to_be_bytes());
    }

    fn sample_and_map(&mut self, rate: u32) {
        self.sample_spec(rate);
        self.channel_map();
    }

    fn sample_spec(&mut self, rate: u32) {
        self.0.extend_from_slice(&[tag::SAMPLE_SPEC, S16LE, STEREO]);
        self.0.extend_from_slice(&rate.to_be_bytes());
    }

    fn channel_map(&mut self) {
        self.0.extend_from_slice(&[tag::CHANNEL_MAP, STEREO]);
        self.0.extend_from_slice(&STEREO_POSITIONS);
    }

    fn volume(&mut self, volume: Volume) {
        self.0.push(tag::VOLUME);
        self.0.extend_from_slice(&volume.as_u32().to_be_bytes());
    }

    fn format(&mut self, props: &Props) -> io::Result<()> {
        self.0.push(tag::FORMAT_INFO);
        self.u8(PCM);
        self.props(props)
    }
}
