//! Sink volume writes and the original channel levels left pending across reconnects.

use pods_core::policy::audio::{AudioRequest, Percent, SinkFact, VolumeStamp};
use pods_proto::addr::BdAddr;
use pulseaudio::protocol::{
    ChannelVolume, Command, CommandTag, SetDeviceVolumeParams, SubscriptionMask, Volume,
};

use super::wire::{self, Malformed};
use super::{Connection, Lost, NO_ENTITY, log};

#[derive(Clone, Copy)]
enum Target {
    Lower(Percent),
    Restore,
}

#[derive(Clone, Copy)]
pub(super) struct VolumeWrite {
    target: Target,
    stamp: VolumeStamp,
    pub(super) retried: bool,
}

impl VolumeWrite {
    pub(super) fn queued(request: AudioRequest) -> Option<Self> {
        let (target, stamp) = match request {
            AudioRequest::SetVolume(percent, stamp) => (Target::Lower(percent), stamp),
            AudioRequest::RestoreVolume(_, stamp) => (Target::Restore, stamp),
            AudioRequest::Follow(_) | AudioRequest::Restore(_) | AudioRequest::CancelRestore => {
                return None;
            }
        };
        Some(Self {
            target,
            stamp,
            retried: false,
        })
    }

    pub(super) fn is_lowering(&self) -> bool {
        matches!(self.target, Target::Lower(_))
    }

    #[cfg(test)]
    pub(super) fn lowered(&self) -> Option<Percent> {
        match self.target {
            Target::Lower(percent) => Some(percent),
            Target::Restore => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Reading {
    pub(super) fact: SinkFact,
    channels: ChannelVolume,
}

#[derive(Clone, Copy)]
pub(super) struct Saved {
    device: BdAddr,
    original: ChannelVolume,
    lowered: Volume,
    restoring: bool,
}

impl Saved {
    fn is_lowered(&self, channels: ChannelVolume) -> bool {
        channels
            .channels()
            .iter()
            .all(|volume| *volume == self.lowered)
    }
}

#[derive(Default)]
pub(super) struct Volumes {
    pub(super) stamp: VolumeStamp,
    pub(super) saved: Vec<Saved>,
}

pub(super) enum Observed {
    Unchanged,
    Adjusted,
    Publish,
}

impl Volumes {
    pub(super) fn reset_stamp(&mut self) {
        self.stamp = VolumeStamp {
            epoch: self.stamp.epoch.saturating_add(1),
            writes: 0,
        };
    }

    pub(super) async fn apply(
        &mut self,
        device: BdAddr,
        pending: &mut Option<VolumeWrite>,
        connection: &mut Connection,
        reading: Option<Reading>,
        stale: &mut SubscriptionMask,
    ) -> Result<Observed, Lost> {
        let mut observed = Observed::Unchanged;
        let position = self.saved.iter().position(|saved| saved.device == device);
        let write = pending.take();
        if let Some(reading) = reading {
            let channels = reading.channels;
            let saved = position.and_then(|position| self.saved.get(position));
            let amount = match write.as_ref().map(|write| write.target) {
                Some(Target::Lower(percent)) => {
                    let lowered = protocol_volume(percent);
                    let original = saved
                        .filter(|saved| saved.restoring && saved.is_lowered(channels))
                        .map_or(channels, |saved| saved.original);
                    let mut volume = ChannelVolume::empty();
                    for _ in channels.channels() {
                        volume.push(lowered);
                    }
                    Some((
                        volume,
                        Saved {
                            device,
                            original,
                            lowered,
                            restoring: false,
                        },
                    ))
                }
                Some(Target::Restore) => saved
                    .filter(|saved| saved.is_lowered(channels) || channels == saved.original)
                    .map(|saved| {
                        let restore = Saved {
                            restoring: true,
                            ..*saved
                        };
                        (restore.original, restore)
                    }),
                None => None,
            };
            if let (Some(write), Some((amount, saved))) = (&write, amount) {
                match connection
                    .set_channel_volume(reading.fact.index, amount, stale)
                    .await
                {
                    Err(Lost::Refused {
                        command: CommandTag::SetSinkVolume,
                        code: NO_ENTITY,
                    }) => {
                        if !write.retried {
                            *stale |= SubscriptionMask::SINK;
                            *pending = Some(VolumeWrite {
                                retried: true,
                                ..*write
                            });
                            return Ok(observed);
                        }
                        log(
                            "audio: volume write dropped after SetSinkVolume returned NoEntity twice",
                        );
                    }
                    result => {
                        // A lost reply can follow a write the server already applied.
                        if let Some(entry) =
                            position.and_then(|position| self.saved.get_mut(position))
                        {
                            *entry = saved;
                        } else {
                            self.saved.push(saved);
                        }
                        result?;
                        if matches!(write.target, Target::Restore) {
                            *stale |= SubscriptionMask::SINK;
                        }
                    }
                }
            } else if write.is_some() && saved.is_none() {
                observed = Observed::Publish;
            } else if let (Some(position), Some(saved)) = (position, saved)
                && ((saved.restoring && channels == saved.original) || !saved.is_lowered(channels))
            {
                observed = if saved.restoring && channels == saved.original {
                    Observed::Publish
                } else {
                    Observed::Adjusted
                };
                self.saved.swap_remove(position);
            }
        }
        // A replaced sink can receive an older request, whose count belongs to its original epoch.
        if let Some(write) = write
            && write.stamp.epoch == self.stamp.epoch
        {
            self.stamp.writes = write.stamp.writes;
        }
        Ok(observed)
    }
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "100 times Volume::NORM plus the rounding offset fits a u32"
)]
pub(super) fn protocol_volume(percent: Percent) -> Volume {
    let full = u32::from(Percent::FULL.get());
    let raw = (u32::from(percent.get()) * Volume::NORM.as_u32() + full / 2) / full;
    Volume::from_u32_clamped(raw)
}

impl Connection {
    pub(super) async fn set_channel_volume(
        &mut self,
        index: u32,
        volume: ChannelVolume,
        stale: &mut SubscriptionMask,
    ) -> Result<(), Lost> {
        let command = Command::SetSinkVolume(SetDeviceVolumeParams {
            device_index: Some(index),
            device_name: None,
            volume,
        });
        self.call(&command, stale, |_| Ok(())).await
    }
}

/// The default matching sink, else the first matching sink, from a native `GetSinkInfoList` reply.
pub(super) fn device_sink(
    body: &[u8],
    prefix: &[u8],
    default: Option<&[u8]>,
) -> Result<Option<Reading>, Malformed> {
    let mut found = None;
    for sink in wire::sinks(body) {
        let sink = sink?;
        if !sink.name.starts_with(prefix) {
            continue;
        }
        let is_default = default == Some(sink.name);
        if is_default || found.is_none() {
            found = Some(Reading {
                fact: SinkFact {
                    index: sink.index,
                    is_default,
                    volume: sink.volume,
                    state: sink.state,
                    volume_stamp: VolumeStamp::default(),
                },
                channels: sink.channels,
            });
        }
    }
    Ok(found)
}
