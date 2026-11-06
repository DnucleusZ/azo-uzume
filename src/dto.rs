use crate::utils::{parse_bool, parse_string};
use crate::{Error, Result, sys};
use std::ffi::CString;
use std::num::NonZeroI32;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelCounts {
    pub in_: i32,
    pub out: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Latencies {
    pub in_: i32,
    pub out: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelInfoResponse {
    pub is_active: bool,
    pub group: sys::ChannelGroupIndex,
    /// Raw sample type; an unknown value does not authorize buffer access.
    pub sample_type: sys::SampleType,
    pub name: CString,
}

impl TryFrom<sys::ChannelInfo> for ChannelInfoResponse {
    type Error = Error;

    fn try_from(value: sys::ChannelInfo) -> Result<Self> {
        if value.channel < 0 || value.channel_group < 0 {
            return Err(Error::InvalidResponse {
                operation: "get_channel_info",
                detail: "negative channel or channel group",
            });
        }
        parse_bool("channel.is_input", value.is_input)?;
        Ok(Self {
            is_active: parse_bool("channel.is_active", value.is_active)?,
            group: value.channel_group,
            sample_type: value.sample_type,
            name: parse_string("channel.name", &value.name)?,
        })
    }
}

/// Raw buffer-size facts. `granularity == None` with `min != max` does not
/// establish a continuous set of supported sizes; the host must not infer one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BufferSize {
    pub min: i32,
    pub max: i32,
    pub preferred: i32,
    pub granularity: Option<Granularity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Granularity {
    /// Supported buffer sizes increase in linear steps (`range.start` + n * `step`)
    Linear { step: NonZeroI32 },
    /// Powers of two within the reported range.
    Exponential,
}

impl TryFrom<NonZeroI32> for Granularity {
    type Error = Error;

    fn try_from(step: NonZeroI32) -> Result<Self> {
        match step.get() {
            -1 => Ok(Self::Exponential),
            1.. => Ok(Self::Linear { step }),
            _ => Err(Error::InvalidResponse {
                operation: "get_buffer_size",
                detail: "granularity must be -1, zero, or a positive step",
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId {
    pub input: bool,
    pub index: sys::ChannelIndex,
}
