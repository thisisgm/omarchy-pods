//! Checked, borrowing readers for native protocol tagstruct fields.

#![forbid(unsafe_code)]

use std::ffi::CStr;
use std::fmt;

use super::{ChannelVolume, MAX_CHANNELS, Volume, tag};

/// Format, channel count and a big-endian sample rate.
const SAMPLE_SPEC_LEN: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagError {
    Cut,
    Tag(u8),
    NoChannels,
    TooManyChannels(u8),
    #[cfg(test)]
    PropertyLength {
        declared: u32,
        actual: usize,
    },
}

impl fmt::Display for TagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cut => f.write_str("a value cut short"),
            Self::Tag(tag) => write!(f, "an unexpected tag 0x{tag:02x}"),
            Self::NoChannels => f.write_str("a volume of no channels"),
            Self::TooManyChannels(count) => {
                write!(f, "a volume of {count} channels, past {MAX_CHANNELS}")
            }
            #[cfg(test)]
            Self::PropertyLength { declared, actual } => {
                write!(f, "property declares {declared} bytes and carries {actual}")
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Tags<'a>(&'a [u8]);

impl<'a> Tags<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn remaining(&self) -> &'a [u8] {
        self.0
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Sample: taking two bytes from `01 02 03` leaves `03`.
    pub(crate) fn take(&mut self, count: usize) -> Result<&'a [u8], TagError> {
        let (taken, rest) = self.0.split_at_checked(count).ok_or(TagError::Cut)?;
        self.0 = rest;
        Ok(taken)
    }

    /// Sample: `ff` reads as 255.
    fn byte(&mut self) -> Result<u8, TagError> {
        let (&byte, rest) = self.0.split_first().ok_or(TagError::Cut)?;
        self.0 = rest;
        Ok(byte)
    }

    /// Sample: `00 00 00 07` reads as 7.
    fn raw_u32(&mut self) -> Result<u32, TagError> {
        let (&bytes, rest) = self.0.split_first_chunk().ok_or(TagError::Cut)?;
        self.0 = rest;
        Ok(u32::from_be_bytes(bytes))
    }

    /// Sample: `L` tags a u32.
    pub(crate) fn expect(&mut self, wanted: u8) -> Result<(), TagError> {
        match self.byte()? {
            got if got == wanted => Ok(()),
            got => Err(TagError::Tag(got)),
        }
    }

    /// Sample: `L 00000007` reads as 7.
    pub(crate) fn u32(&mut self) -> Result<u32, TagError> {
        self.expect(tag::U32)?;
        self.raw_u32()
    }

    /// Sample: `B 01` reads as 1.
    pub(crate) fn u8(&mut self) -> Result<u8, TagError> {
        self.expect(tag::U8)?;
        self.byte()
    }

    /// Sample: `1` is true and `0` is false.
    pub(crate) fn boolean(&mut self) -> Result<bool, TagError> {
        match self.byte()? {
            tag::TRUE => Ok(true),
            tag::FALSE => Ok(false),
            got => Err(TagError::Tag(got)),
        }
    }

    /// Sample: `t pulse\0` names pulse and `N` is null.
    pub(crate) fn string(&mut self) -> Result<Option<&'a CStr>, TagError> {
        match self.byte()? {
            tag::STRING_NULL => Ok(None),
            tag::STRING => self.c_string().map(Some),
            got => Err(TagError::Tag(got)),
        }
    }

    /// Sample: `pulse\0` consumes six bytes.
    fn c_string(&mut self) -> Result<&'a CStr, TagError> {
        let text = CStr::from_bytes_until_nul(self.0).map_err(|_error| TagError::Cut)?;
        self.take(text.to_bytes_with_nul().len())?;
        Ok(text)
    }

    /// Sample: `x 00000002 ff 00` yields two bytes.
    pub(crate) fn arbitrary(&mut self) -> Result<&'a [u8], TagError> {
        self.expect(tag::ARBITRARY)?;
        self.raw_arbitrary()
    }

    /// Sample: `00000002 ff 00` yields two bytes after the length.
    fn raw_arbitrary(&mut self) -> Result<&'a [u8], TagError> {
        let length = usize::try_from(self.raw_u32()?).map_err(|_error| TagError::Cut)?;
        self.take(length)
    }

    /// Sample: `v 02 00008000 00010000` gives two channels and an unclamped sum of 98304.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "at most 32 u32 channel values fit in a u64 sum"
    )]
    pub(crate) fn channels(&mut self) -> Result<(ChannelVolume, u64), TagError> {
        self.expect(tag::CVOLUME)?;
        let count = self.byte()?;
        if count == 0 {
            return Err(TagError::NoChannels);
        }
        if usize::from(count) > MAX_CHANNELS {
            return Err(TagError::TooManyChannels(count));
        }
        let mut volume = ChannelVolume::empty();
        let mut sum = 0_u64;
        for _ in 0..count {
            let raw = self.raw_u32()?;
            volume.push(Volume::from_u32_clamped(raw));
            sum += u64::from(raw);
        }
        Ok((volume, sum))
    }

    /// Sample: `P t key\0 L 00000002 x 00000002 v\0 N` passes one property to the callback.
    pub(crate) fn properties(
        &mut self,
        see: impl FnMut(&'a CStr, u32, &'a [u8]) -> Result<(), TagError>,
    ) -> Result<(), TagError> {
        self.expect(tag::PROPLIST)?;
        self.property_entries(see)
    }

    /// Sample: `t key\0 L 00000002 x 00000002 v\0 N` is a property list after its tag.
    fn property_entries(
        &mut self,
        mut see: impl FnMut(&'a CStr, u32, &'a [u8]) -> Result<(), TagError>,
    ) -> Result<(), TagError> {
        while let Some(key) = self.string()? {
            let length = self.u32()?;
            let value = self.arbitrary()?;
            see(key, length, value)?;
        }
        Ok(())
    }

    /// Sample: `L 00000005` or `t name\0` each skips one field.
    pub(crate) fn skip(&mut self) -> Result<(), TagError> {
        let field = self.byte()?;
        self.skip_value(field)
    }

    /// Sample: `m 02 01 02` is skipped when a channel map is expected.
    #[cfg(test)]
    pub(crate) fn skip_expected(&mut self, field: u8) -> Result<(), TagError> {
        self.expect(field)?;
        self.skip_value(field)
    }

    /// Sample: after tag `L`, four bytes encode the skipped integer.
    fn skip_value(&mut self, field: u8) -> Result<(), TagError> {
        let length = match field {
            tag::STRING => return self.c_string().map(|_| ()),
            tag::ARBITRARY => return self.raw_arbitrary().map(|_| ()),
            tag::PROPLIST => return self.property_entries(|_, _, _| Ok(())),
            tag::FORMAT_INFO => return self.format_fields(|_, _, _| Ok(())),
            tag::STRING_NULL | tag::TRUE | tag::FALSE => 0,
            tag::U8 => size_of::<u8>(),
            tag::U32 | tag::VOLUME => size_of::<u32>(),
            tag::U64 | tag::S64 | tag::USEC | tag::TIMEVAL => size_of::<u64>(),
            tag::SAMPLE_SPEC => SAMPLE_SPEC_LEN,
            tag::CHANNEL_MAP => usize::from(self.byte()?),
            tag::CVOLUME => usize::from(self.byte()?).saturating_mul(size_of::<u32>()),
            got => return Err(TagError::Tag(got)),
        };
        self.take(length).map(|_| ())
    }

    /// Sample: skipping two fields in `L 00000001 N L 00000002` leaves the second integer.
    pub(crate) fn skip_n(&mut self, count: usize) -> Result<(), TagError> {
        for _ in 0..count {
            self.skip()?;
        }
        Ok(())
    }

    /// Sample: `f B 01 P N` is PCM with no format properties.
    #[cfg(test)]
    pub(crate) fn format(
        &mut self,
        see: impl FnMut(&'a CStr, u32, &'a [u8]) -> Result<(), TagError>,
    ) -> Result<(), TagError> {
        self.expect(tag::FORMAT_INFO)?;
        self.format_fields(see)
    }

    /// Sample: `B 01 P N` is PCM with no properties after its format tag.
    fn format_fields(
        &mut self,
        see: impl FnMut(&'a CStr, u32, &'a [u8]) -> Result<(), TagError>,
    ) -> Result<(), TagError> {
        self.u8()?;
        self.properties(see)
    }

    /// Sample: `a 03 02 0000bb80` is stereo S16LE at 48 kHz.
    #[cfg(test)]
    pub(crate) fn sample_rate(&mut self) -> Result<u32, TagError> {
        self.expect(tag::SAMPLE_SPEC)?;
        self.byte()?;
        self.byte()?;
        self.raw_u32()
    }
}
