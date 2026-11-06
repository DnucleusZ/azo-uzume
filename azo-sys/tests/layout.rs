//! Fixed Windows SDK ABI facts. The independent C++ probe lives in sdk_abi.rs.

use azo_sys::{utils::I64Split, *};
use std::mem::{align_of, offset_of, size_of};

macro_rules! layout {
    ($ty:ty, $size:expr, $align:expr; $($field:ident: $offset:expr),* $(,)?) => {
        assert_eq!(size_of::<$ty>(), $size, "{} size", stringify!($ty));
        assert_eq!(align_of::<$ty>(), $align, "{} alignment", stringify!($ty));
        $(assert_eq!(offset_of!($ty, $field), $offset,
            "{}::{} offset", stringify!($ty), stringify!($field));)*
    };
}

#[test]
fn windows_scalar_layouts() {
    // Windows SDK long/unsigned long 始终为 32 位，不随指针宽度增长。
    for (size, alignment) in [
        (size_of::<Bool>(), align_of::<Bool>()),
        (size_of::<SampleType>(), align_of::<SampleType>()),
        (size_of::<ResultCode>(), align_of::<ResultCode>()),
        (size_of::<FutureSelector>(), align_of::<FutureSelector>()),
        (size_of::<MessageSelector>(), align_of::<MessageSelector>()),
        (
            size_of::<TransportParametersCommand>(),
            align_of::<TransportParametersCommand>(),
        ),
        (size_of::<IoFormatType>(), align_of::<IoFormatType>()),
        (size_of::<TimeInfoFlags>(), align_of::<TimeInfoFlags>()),
        (size_of::<TimeCodeFlags>(), align_of::<TimeCodeFlags>()),
    ] {
        assert_eq!((size, alignment), (4, 4));
    }
    layout!(I64Split, 8, 4; hi: 0, lo: 4);
    assert_eq!(size_of::<SampleRate>(), 8);
}

#[test]
fn time_layouts_use_four_byte_packing() {
    layout!(TimeCode, 84, 4;
        speed: 0, time_code_samples: 8, flags: 16, _placeholder: 20);
    layout!(TimeInfo, 48, 4;
        speed: 0, system_time: 8, sample_position: 16,
        sample_rate: 24, flags: 32, _reserved: 36);
    layout!(Time, 148, 4; _reserved: 0, time_info: 16, time_code: 64);
}

#[test]
fn channel_and_extension_layouts() {
    layout!(ClockSource, 48, 4;
        index: 0, associated_channel: 4, associated_group: 8,
        is_current_source: 12, name: 16);
    layout!(ChannelInfo, 52, 4;
        channel: 0, is_input: 4, is_active: 8, channel_group: 12,
        sample_type: 16, name: 20);
    layout!(BufferInfo, 24, 4; is_input: 0, channel_num: 4, buffers: 8);
    layout!(InputMonitor, 20, 4; input: 0, output: 4, gain: 8, state: 12, pan: 16);
    layout!(IoFormat, 512, 4; format_type: 0, _placeholder: 4);
    layout!(InternalBufferInfo, 8, 4; input_samples: 0, output_samples: 4);
}

#[cfg(feature = "undocumented")]
#[test]
fn undocumented_layouts() {
    layout!(ChannelControls, 48, 4;
        channel: 0, is_input: 4, gain: 8, meter: 12, _placeholder: 16);
    layout!(TransportParameters, 144, 4;
        command: 0, sample_position: 4, track: 12, track_switches: 16,
        _placeholder: 80);
}

#[test]
fn callback_layout_and_nullable_time_callback() {
    layout!(Callbacks, 32, 4;
        buffer_switch: 0, sample_rate_did_change: 8,
        asio_message: 16, buffer_switch_time_info: 24);
    assert_eq!(size_of::<Option<BufferSwitchTimeInfo>>(), 8);
    let callbacks = Callbacks {
        buffer_switch_time_info: None,
        ..Callbacks::NOOP
    };
    // Option<extern fn> 的空值必须能直接交给未协商 time-info 的 C++ driver。
    let absent = callbacks.buffer_switch_time_info;
    let raw: usize = unsafe { std::mem::transmute(absent) };
    assert_eq!(raw, 0);
}

#[test]
fn iasio_vtable_slots_follow_iunknown() {
    layout!(IIASIORedecl_Vtbl, 24 * 8, 8;
        base__: 0,
        init: 3 * 8,
        get_driver_name: 4 * 8,
        get_driver_version: 5 * 8,
        get_error_message: 6 * 8,
        start: 7 * 8,
        stop: 8 * 8,
        get_channels: 9 * 8,
        get_latencies: 10 * 8,
        get_buffer_size: 11 * 8,
        can_sample_rate: 12 * 8,
        get_sample_rate: 13 * 8,
        set_sample_rate: 14 * 8,
        get_clock_sources: 15 * 8,
        set_clock_source: 16 * 8,
        get_sample_position: 17 * 8,
        get_channel_info: 18 * 8,
        create_buffers: 19 * 8,
        dispose_buffers: 20 * 8,
        control_panel: 21 * 8,
        future: 22 * 8,
        output_ready: 23 * 8);
}

#[test]
fn packed_values_work_at_four_byte_aligned_addresses() {
    #[repr(C, align(8))]
    struct Storage<T> {
        prefix: u32,
        value: T,
    }

    let storage = Storage {
        prefix: 0,
        value: Time {
            time_info: TimeInfo {
                speed: 1.25,
                sample_rate: 192_000.0,
                ..TimeInfo::default()
            },
            time_code: TimeCode {
                speed: 0.75,
                ..TimeCode::invalid()
            },
            ..Time::default()
        },
    };
    assert_eq!(std::ptr::addr_of!(storage.value) as usize % 8, 4);
    let value = storage.value;
    assert_eq!({ value.time_info.speed }, 1.25);
    assert_eq!({ value.time_info.sample_rate }, 192_000.0);
    assert_eq!({ value.time_code.speed }, 0.75);
    assert_eq!(storage.value, value);
    assert!(format!("{:?}", storage.value).contains("192000"));

    let buffers = Storage {
        prefix: 0,
        value: BufferInfo {
            is_input: Bool::FALSE,
            channel_num: 1,
            buffers: [std::ptr::null_mut(); 2],
        },
    };
    assert_eq!(std::ptr::addr_of!(buffers.value) as usize % 8, 4);
    let pointers = buffers.value.buffers;
    assert!(pointers.iter().all(|pointer| pointer.is_null()));
    // 派生实现也必须先复制 packed 指针字段，不能建立 8 字节对齐的引用。
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&buffers.value, &mut hasher);
    assert!(format!("{:?}", buffers.value).contains("channel_num: 1"));
}
