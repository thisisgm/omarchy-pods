//! Reply records emitted by the fake server, with only the variable fields its tests exercise.

use std::ffi::{CStr, CString};
use std::io;

use super::tags::Tags;
use super::{
    ChannelVolume, FLAG_MEMFD, FLAG_SHM, Props, ProtocolError, Result, SinkState, TagWriter,
    Volume, field_length, protocol_version, tag, version_flags,
};

const SINK_RATE: u32 = 48_000;
const SINK_PORT: &CStr = c"Stereo Output";
const DEVICE_STATE_VERSION: u16 = 15;
const DEVICE_PORTS_VERSION: u16 = 16;
const DEVICE_FORMATS_VERSION: u16 = 21;
const PORT_AVAILABILITY_VERSION: u16 = 24;
const CARD_PORTS_VERSION: u16 = 26;
const PORT_OFFSET_VERSION: u16 = 27;
const PROFILE_AVAILABILITY_VERSION: u16 = 29;
const PORT_TYPE_VERSION: u16 = 34;
const SOURCE_PROPS_VERSION: u16 = 13;
const RECORDER_CORKED_VERSION: u16 = 19;
const RECORDER_VOLUME_VERSION: u16 = 22;

pub(crate) trait CommandReply: Sized {
    fn write(&self, writer: &mut TagWriter<'_>, version: u16) -> io::Result<()>;
    fn read(reader: &mut Tags<'_>, version: u16) -> Result<Self>;
}

impl<T: CommandReply> CommandReply for Vec<T> {
    fn write(&self, writer: &mut TagWriter<'_>, version: u16) -> io::Result<()> {
        for record in self {
            record.write(writer, version)?;
        }
        Ok(())
    }

    // Sample: a sequence of adjacent records without a list count.
    fn read(reader: &mut Tags<'_>, version: u16) -> Result<Self> {
        let mut records = Vec::new();
        while !reader.is_empty() {
            records.push(T::read(reader, version)?);
        }
        Ok(records)
    }
}

#[derive(Debug, Default)]
pub(crate) struct AuthReply {
    pub(crate) version: u16,
    pub(crate) use_memfd: bool,
    pub(crate) use_shm: bool,
}

impl CommandReply for AuthReply {
    fn write(&self, writer: &mut TagWriter<'_>, _: u16) -> io::Result<()> {
        writer.u32(version_flags(self.version, self.use_shm, self.use_memfd));
        Ok(())
    }

    // Sample: L 40000023 means version 35 with memfd.
    fn read(reader: &mut Tags<'_>, _: u16) -> Result<Self> {
        let raw = reader.u32()?;
        Ok(Self {
            version: protocol_version(raw),
            use_memfd: raw & FLAG_MEMFD != 0,
            use_shm: raw & FLAG_SHM != 0,
        })
    }
}

#[derive(Debug)]
pub(crate) struct SetClientNameReply {
    pub(crate) client_id: u32,
}

impl CommandReply for SetClientNameReply {
    fn write(&self, writer: &mut TagWriter<'_>, _: u16) -> io::Result<()> {
        writer.u32(self.client_id);
        Ok(())
    }

    // Sample: L 00000007 assigns client ID 7.
    fn read(reader: &mut Tags<'_>, _: u16) -> Result<Self> {
        Ok(Self {
            client_id: reader.u32()?,
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct ServerInfo {
    pub(crate) server_name: Option<CString>,
    pub(crate) default_sink: Option<CString>,
    pub(crate) default_source: Option<CString>,
}

impl CommandReply for ServerInfo {
    fn write(&self, writer: &mut TagWriter<'_>, _: u16) -> io::Result<()> {
        const SAMPLE_FORMAT_UNSPECIFIED: u8 = u8::MAX;
        const MONO: u8 = 0;
        writer.string(self.server_name.as_deref());
        writer.string(None);
        writer.string(None);
        writer.string(None);
        writer
            .0
            .extend_from_slice(&[tag::SAMPLE_SPEC, SAMPLE_FORMAT_UNSPECIFIED, 0]);
        writer.0.extend_from_slice(&0_u32.to_be_bytes());
        writer.string(self.default_sink.as_deref());
        writer.string(self.default_source.as_deref());
        writer.u32(0);
        writer.0.extend_from_slice(&[tag::CHANNEL_MAP, 1, MONO]);
        Ok(())
    }

    // Sample: N N N N a 03 02 0000ac44 t sink\0 N L 00000000 m 02 01 02.
    fn read(reader: &mut Tags<'_>, _: u16) -> Result<Self> {
        let server_name = reader.owned_string()?;
        reader.owned_string()?;
        reader.owned_string()?;
        reader.owned_string()?;
        reader.sample_rate()?;
        let default_sink = reader.owned_string()?;
        let default_source = reader.owned_string()?;
        reader.u32()?;
        reader.skip_expected(tag::CHANNEL_MAP)?;
        Ok(Self {
            server_name,
            default_sink,
            default_source,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SinkInfo {
    pub(crate) index: u32,
    pub(crate) name: CString,
    pub(crate) cvolume: ChannelVolume,
    pub(crate) state: SinkState,
    pub(crate) monitor_source_index: Option<u32>,
    pub(crate) monitor_source_name: Option<CString>,
    pub(crate) props: Props,
    pub(crate) formats: Vec<Props>,
}

impl SinkInfo {
    pub(crate) fn new_dummy(index: u32) -> Self {
        Self {
            index,
            name: c"Dummy Sink".to_owned(),
            cvolume: ChannelVolume::norm(2),
            state: SinkState::Idle,
            monitor_source_index: None,
            monitor_source_name: None,
            props: Props::new(),
            formats: vec![Props::new()],
        }
    }
}

impl CommandReply for SinkInfo {
    fn write(&self, writer: &mut TagWriter<'_>, version: u16) -> io::Result<()> {
        write_device(writer, self.index, &self.name, SINK_RATE, &self.cvolume);
        writer.index(self.monitor_source_index);
        writer.string(self.monitor_source_name.as_deref());
        write_latency_driver_flags(writer);
        writer.props(&self.props)?;
        writer.latency();
        write_device_state(writer, self.state, version, Some(SINK_PORT));
        if version >= DEVICE_FORMATS_VERSION {
            writer.u8(u8::try_from(self.formats.len()).map_err(io::Error::other)?);
            for format in &self.formats {
                writer.format(format)?;
            }
        }
        Ok(())
    }

    // Sample: L 0 t sink\0 N a 03 02 0000ac44 m 02 01 02 L ffffffff v 02 00010000 00010000 0 L ffffffff N U 0 N L 0 P N U 0 V 00010000 L 1 L 0 L ffffffff L 0 N B 0.
    fn read(reader: &mut Tags<'_>, version: u16) -> Result<Self> {
        let (index, name, cvolume, _) = read_device(reader)?;
        let monitor_source_index = reader.index()?;
        let monitor_source_name = reader.owned_string()?;
        read_latency_driver_flags(reader)?;
        let props = reader.props()?;
        reader.skip_expected(tag::USEC)?;
        let state = read_device_state(reader, version, true)?;
        let mut formats = Vec::new();
        if version >= DEVICE_FORMATS_VERSION {
            for _ in 0..reader.u8()? {
                formats.push(reader.format_props()?);
            }
        }
        Ok(Self {
            index,
            name,
            cvolume,
            state,
            monitor_source_index,
            monitor_source_name,
            props,
            formats,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SourceInfo {
    pub(crate) index: u32,
    pub(crate) name: CString,
    pub(crate) sample_rate: u32,
    pub(crate) cvolume: ChannelVolume,
    pub(crate) monitor_of_sink_index: Option<u32>,
    pub(crate) monitor_of_sink_name: Option<CString>,
}

impl CommandReply for SourceInfo {
    fn write(&self, writer: &mut TagWriter<'_>, version: u16) -> io::Result<()> {
        write_device(
            writer,
            self.index,
            &self.name,
            self.sample_rate,
            &self.cvolume,
        );
        writer.index(self.monitor_of_sink_index);
        writer.string(self.monitor_of_sink_name.as_deref());
        write_latency_driver_flags(writer);
        if version >= SOURCE_PROPS_VERSION {
            writer.props(&Props::new())?;
            writer.latency();
        }
        write_device_state(writer, SinkState::Suspended, version, None);
        if version >= DEVICE_FORMATS_VERSION {
            writer.u8(0);
        }
        Ok(())
    }

    // Sample: the sink record above with monitor index and name identifying a sink.
    fn read(reader: &mut Tags<'_>, version: u16) -> Result<Self> {
        let (index, name, cvolume, sample_rate) = read_device(reader)?;
        let monitor_of_sink_index = reader.index()?;
        let monitor_of_sink_name = reader.owned_string()?;
        read_latency_driver_flags(reader)?;
        if version >= SOURCE_PROPS_VERSION {
            reader.props()?;
            reader.skip_expected(tag::USEC)?;
        }
        read_device_state(reader, version, false)?;
        if version >= DEVICE_FORMATS_VERSION {
            for _ in 0..reader.u8()? {
                reader.format_props()?;
            }
        }
        Ok(Self {
            index,
            name,
            sample_rate,
            cvolume,
            monitor_of_sink_index,
            monitor_of_sink_name,
        })
    }
}

fn write_device(
    writer: &mut TagWriter<'_>,
    index: u32,
    name: &CStr,
    rate: u32,
    volume: &ChannelVolume,
) {
    writer.u32(index);
    writer.string(Some(name));
    writer.string(None);
    writer.sample_and_map(rate);
    writer.index(None);
    writer.channels(volume);
    writer.boolean(false);
}

// Sample: L 0 t sink\0 N a 03 02 0000ac44 m 02 01 02 L ffffffff v 01 00010000 0.
fn read_device(reader: &mut Tags<'_>) -> Result<(u32, CString, ChannelVolume, u32)> {
    let index = reader.u32()?;
    let name = reader.name()?;
    reader.owned_string()?;
    let sample_rate = reader.sample_and_map()?;
    reader.index()?;
    let volume = reader.channel_volume()?;
    reader.boolean()?;
    Ok((index, name, volume, sample_rate))
}

fn write_latency_driver_flags(writer: &mut TagWriter<'_>) {
    writer.latency();
    writer.string(None);
    writer.u32(0);
}

// Sample: U 0000000000000000 N L 00000000 means zero latency and flags and no driver.
fn read_latency_driver_flags(reader: &mut Tags<'_>) -> Result<()> {
    reader.skip_expected(tag::USEC)?;
    reader.owned_string()?;
    reader.u32()?;
    Ok(())
}

fn write_device_state(
    writer: &mut TagWriter<'_>,
    state: SinkState,
    version: u16,
    port: Option<&CStr>,
) {
    if version >= DEVICE_STATE_VERSION {
        writer.volume(Volume::MUTED);
        writer.u32(state.number());
        writer.u32(0);
        writer.index(None);
    }
    if version >= DEVICE_PORTS_VERSION {
        writer.u32(u32::from(port.is_some()));
        if let Some(name) = port {
            const PORT_AVAILABLE: u32 = 2;
            writer.string(Some(name));
            writer.string(None);
            writer.u32(0);
            if version >= PORT_AVAILABILITY_VERSION {
                writer.u32(PORT_AVAILABLE);
            }
            if version >= PORT_TYPE_VERSION {
                writer.string(None);
                writer.u32(0);
            }
        }
        writer.string(port);
    }
}

// Sample: V 00010000 L 00000001 L 00000000 L ffffffff L 00000000 N means idle with no card or ports.
fn read_device_state(reader: &mut Tags<'_>, version: u16, sink: bool) -> Result<SinkState> {
    let mut state = SinkState::Idle;
    if version >= DEVICE_STATE_VERSION {
        reader.skip_expected(tag::VOLUME)?;
        state = match reader.u32()? {
            n if n == SinkState::Running.number() => SinkState::Running,
            n if n == SinkState::Idle.number() => SinkState::Idle,
            n if n == SinkState::Suspended.number() => SinkState::Suspended,
            _ => return Err(ProtocolError::Invalid("invalid PulseAudio sink state")),
        };
        reader.u32()?;
        reader.index()?;
    }
    if version >= DEVICE_PORTS_VERSION {
        for _ in 0..reader.u32()? {
            reader.name()?;
            reader.owned_string()?;
            reader.u32()?;
            if version >= PORT_AVAILABILITY_VERSION {
                reader.u32()?;
            }
            if sink && version >= PORT_TYPE_VERSION {
                reader.owned_string()?;
                reader.u32()?;
            }
        }
        reader.owned_string()?;
    }
    Ok(state)
}

#[derive(Debug, Clone)]
pub(crate) struct SourceOutputInfo {
    pub(crate) index: u32,
    pub(crate) name: CString,
    pub(crate) source_index: u32,
    pub(crate) sample_rate: u32,
    pub(crate) cvolume: ChannelVolume,
    pub(crate) props: Props,
    pub(crate) corked: bool,
}

impl CommandReply for SourceOutputInfo {
    fn write(&self, writer: &mut TagWriter<'_>, version: u16) -> io::Result<()> {
        writer.u32(self.index);
        writer.string(Some(&self.name));
        writer.index(None);
        writer.index(None);
        writer.u32(self.source_index);
        writer.sample_and_map(self.sample_rate);
        writer.latency();
        writer.latency();
        writer.string(None);
        writer.string(None);
        writer.props(&self.props)?;
        if version >= RECORDER_CORKED_VERSION {
            writer.boolean(self.corked);
        }
        if version >= RECORDER_VOLUME_VERSION {
            writer.channels(&self.cvolume);
            writer.boolean(false);
            writer.boolean(true);
            writer.boolean(true);
            writer.format(&Props::new())?;
        }
        Ok(())
    }

    // Sample: L 0 t recorder\0 L ffffffff L ffffffff L 7 a 03 02 0000ac44 m 02 01 02 U 0 U 0 N N P N 0 v 02 00010000 00010000 0 1 1 f B 01 P N.
    fn read(reader: &mut Tags<'_>, version: u16) -> Result<Self> {
        let index = reader.u32()?;
        let name = reader.name()?;
        reader.index()?;
        reader.index()?;
        let source_index = reader.u32()?;
        let sample_rate = reader.sample_and_map()?;
        reader.skip_expected(tag::USEC)?;
        reader.skip_expected(tag::USEC)?;
        reader.owned_string()?;
        reader.owned_string()?;
        let props = reader.props()?;
        let corked = version >= RECORDER_CORKED_VERSION && reader.boolean()?;
        let cvolume = if version >= RECORDER_VOLUME_VERSION {
            let volume = reader.channel_volume()?;
            reader.boolean()?;
            reader.boolean()?;
            reader.boolean()?;
            reader.format_props()?;
            volume
        } else {
            ChannelVolume::empty()
        };
        Ok(Self {
            index,
            name,
            source_index,
            sample_rate,
            cvolume,
            props,
            corked,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CardInfo {
    pub(crate) index: u32,
    pub(crate) name: CString,
    pub(crate) props: Props,
    pub(crate) ports: Vec<CardPortInfo>,
    pub(crate) profiles: Vec<CardProfileInfo>,
    pub(crate) active_profile: Option<CString>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CardProfileInfo {
    pub(crate) name: CString,
    pub(crate) num_sinks: u32,
    pub(crate) num_sources: u32,
    pub(crate) available: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CardPortInfo {
    pub(crate) name: CString,
    pub(crate) props: Props,
    pub(crate) availability_group: Option<CString>,
    pub(crate) profiles: Vec<CString>,
}

impl CommandReply for CardInfo {
    fn write(&self, writer: &mut TagWriter<'_>, version: u16) -> io::Result<()> {
        writer.u32(self.index);
        writer.string(Some(&self.name));
        writer.index(None);
        writer.string(None);
        writer.u32(field_length(self.profiles.len())?);
        for profile in &self.profiles {
            writer.string(Some(&profile.name));
            writer.string(None);
            writer.u32(profile.num_sinks);
            writer.u32(profile.num_sources);
            writer.u32(0);
            if version >= PROFILE_AVAILABILITY_VERSION {
                writer.u32(profile.available);
            }
        }
        writer.string(self.active_profile.as_deref());
        writer.props(&self.props)?;
        if version >= CARD_PORTS_VERSION {
            writer.u32(field_length(self.ports.len())?);
            for port in &self.ports {
                port.write(writer, version)?;
            }
        }
        Ok(())
    }

    // Sample: L 0 t card\0 L ffffffff N L 0 N P N L 0 is a card with no profiles or ports.
    fn read(reader: &mut Tags<'_>, version: u16) -> Result<Self> {
        let index = reader.u32()?;
        let name = reader.name()?;
        reader.index()?;
        reader.owned_string()?;
        let mut profiles = Vec::new();
        for _ in 0..reader.u32()? {
            let name = reader.name()?;
            reader.owned_string()?;
            let num_sinks = reader.u32()?;
            let num_sources = reader.u32()?;
            reader.u32()?;
            let available = if version >= PROFILE_AVAILABILITY_VERSION {
                reader.u32()?
            } else {
                0
            };
            profiles.push(CardProfileInfo {
                name,
                num_sinks,
                num_sources,
                available,
            });
        }
        let active_profile = reader.owned_string()?;
        let props = reader.props()?;
        let mut ports = Vec::new();
        if version >= CARD_PORTS_VERSION {
            for _ in 0..reader.u32()? {
                ports.push(CardPortInfo::read(reader, version)?);
            }
        }
        Ok(Self {
            index,
            name,
            props,
            ports,
            profiles,
            active_profile,
        })
    }
}

impl CardPortInfo {
    fn write(&self, writer: &mut TagWriter<'_>, version: u16) -> io::Result<()> {
        const OUTPUT: u8 = 2;
        writer.string(Some(&self.name));
        writer.string(None);
        writer.u32(0);
        writer.u32(0);
        writer.u8(OUTPUT);
        writer.props(&self.props)?;
        writer.u32(field_length(self.profiles.len())?);
        for profile in &self.profiles {
            writer.string(Some(profile));
        }
        if version >= PORT_OFFSET_VERSION {
            writer.0.push(tag::S64);
            writer.0.extend_from_slice(&0_i64.to_be_bytes());
        }
        if version >= PORT_TYPE_VERSION {
            writer.string(self.availability_group.as_deref());
            writer.u32(0);
        }
        Ok(())
    }

    // Sample: t output\0 N L 0 L 0 B 01 P N L 0 r 0000000000000000 N L 0 is a bare output port.
    fn read(reader: &mut Tags<'_>, version: u16) -> Result<Self> {
        let name = reader.name()?;
        reader.owned_string()?;
        reader.u32()?;
        reader.u32()?;
        reader.u8()?;
        let props = reader.props()?;
        let mut profiles = Vec::new();
        for _ in 0..reader.u32()? {
            profiles.push(reader.name()?);
        }
        let mut availability_group = None;
        if version >= PORT_OFFSET_VERSION {
            reader.skip_expected(tag::S64)?;
        }
        if version >= PORT_TYPE_VERSION {
            availability_group = reader.owned_string()?;
            reader.u32()?;
        }
        Ok(Self {
            name,
            props,
            availability_group,
            profiles,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ClientInfo {
    pub(crate) index: u32,
    pub(crate) name: CString,
    pub(crate) props: Props,
}

impl CommandReply for ClientInfo {
    fn write(&self, writer: &mut TagWriter<'_>, _: u16) -> io::Result<()> {
        writer.u32(self.index);
        writer.string(Some(&self.name));
        writer.index(None);
        writer.string(None);
        writer.props(&self.props)
    }

    // Sample: L 0 t Recorder\0 L ffffffff N P N gives a client without properties.
    fn read(reader: &mut Tags<'_>, _: u16) -> Result<Self> {
        let index = reader.u32()?;
        let name = reader.name()?;
        reader.index()?;
        reader.owned_string()?;
        Ok(Self {
            index,
            name,
            props: reader.props()?,
        })
    }
}
