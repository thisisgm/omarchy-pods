//! Ownership and validation for fixture values, plus additional tag writers.

#![forbid(unsafe_code)]

use std::ffi::{CStr, CString};
use std::io;

use super::tags::{TagError, Tags};
use super::{
    ChannelVolume, DeviceLookup, INDEX_NONE, Props, ProtocolError, Result, TagWriter, Volume, tag,
};

pub(super) trait FixtureFields {
    fn finish(&self) -> Result<()>;
    fn index(&mut self) -> Result<Option<u32>>;
    fn owned_string(&mut self) -> Result<Option<CString>>;
    fn name(&mut self) -> Result<CString>;
    fn device(&mut self) -> Result<DeviceLookup>;
    fn props(&mut self) -> Result<Props>;
    fn channel_volume(&mut self) -> Result<ChannelVolume>;
    fn sample_and_map(&mut self) -> Result<u32>;
    fn format_props(&mut self) -> Result<Props>;
}

impl FixtureFields for Tags<'_> {
    fn finish(&self) -> Result<()> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::Invalid(
                "trailing bytes in PulseAudio message",
            ))
        }
    }

    // Sample: L ffffffff represents an absent index.
    fn index(&mut self) -> Result<Option<u32>> {
        let index = self.u32()?;
        Ok((index != INDEX_NONE).then_some(index))
    }

    // Sample: t sink\0 yields an owned name.
    fn owned_string(&mut self) -> Result<Option<CString>> {
        Ok(self.string()?.map(CStr::to_owned))
    }

    // Sample: t sink\0 yields a required name.
    fn name(&mut self) -> Result<CString> {
        self.owned_string()?
            .ok_or(ProtocolError::Invalid("missing PulseAudio name"))
    }

    // Sample: L ffffffff t sink\0 selects a sink by name.
    fn device(&mut self) -> Result<DeviceLookup> {
        Ok(DeviceLookup {
            index: self.index()?,
            name: self.owned_string()?,
        })
    }

    // Sample: P t key\0 L 00000002 x 00000002 v\0 N yields one owned property.
    fn props(&mut self) -> Result<Props> {
        let mut props = Props::new();
        self.properties(|key, declared, value| store_property(&mut props, key, declared, value))?;
        Ok(props)
    }

    // Sample: v 02 00010000 00008000 yields two channel volumes.
    fn channel_volume(&mut self) -> Result<ChannelVolume> {
        Ok(self.channels()?.0)
    }

    // Sample: a 03 02 0000bb80 m 02 01 02 is stereo S16LE at 48 kHz.
    fn sample_and_map(&mut self) -> Result<u32> {
        let rate = self.sample_rate()?;
        self.skip_expected(tag::CHANNEL_MAP)?;
        Ok(rate)
    }

    // Sample: f B 01 P N is PCM with no format properties.
    fn format_props(&mut self) -> Result<Props> {
        let mut props = Props::new();
        self.format(|key, declared, value| store_property(&mut props, key, declared, value))?;
        Ok(props)
    }
}

fn store_property(
    props: &mut Props,
    key: &CStr,
    declared: u32,
    value: &[u8],
) -> std::result::Result<(), TagError> {
    if usize::try_from(declared) != Ok(value.len()) {
        return Err(TagError::PropertyLength {
            declared,
            actual: value.len(),
        });
    }
    props.set_bytes(key, value);
    Ok(())
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
