//! External SDK oracle and native C++ round trips; no SDK code is included here.
//! Run explicitly with AZO_SDK_ABI_DLL set and `--ignored --nocapture`.
#![cfg(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc"))]

use azo_sys::*;
use std::{
    ffi::{CStr, CString, c_void},
    mem::{align_of, offset_of, size_of},
    os::windows::ffi::OsStrExt,
    ptr,
    sync::atomic::{AtomicU32, Ordering},
};
use windows_core::{GUID, Interface};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(path: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

struct Probe(*mut c_void);
impl Probe {
    fn load() -> Self {
        let path = std::env::var_os("AZO_SDK_ABI_DLL")
            .expect("AZO_SDK_ABI_DLL must identify the local SDK probe DLL");
        assert!(
            std::path::Path::new(&path).is_absolute(),
            "probe path must be absolute"
        );
        let wide: Vec<u16> = path.encode_wide().chain(Some(0)).collect();
        // SAFETY: absolute, explicitly supplied local test DLL; the path is NUL terminated.
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        assert!(
            !module.is_null(),
            "LoadLibraryW({path:?}): {}",
            std::io::Error::last_os_error()
        );
        println!("sdk_probe={}", path.to_string_lossy());
        Self(module)
    }
    unsafe fn symbol<T: Copy>(&self, name: &CStr) -> T {
        // SAFETY: the caller provides the export's complete C ABI signature.
        let address = unsafe { GetProcAddress(self.0, name.as_ptr().cast()) };
        assert!(!address.is_null(), "missing export {name:?}");
        assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
        unsafe { std::mem::transmute_copy(&address) }
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        // SAFETY: all fixture objects and callback calls finish before unloading.
        unsafe { FreeLibrary(self.0) };
    }
}

static SEEN: AtomicU32 = AtomicU32::new(0);
static ERRORS: AtomicU32 = AtomicU32::new(0);
static SWITCHES: AtomicU32 = AtomicU32::new(0);
fn callback_result(bit: u32, good: bool) {
    SEEN.fetch_or(bit, Ordering::SeqCst);
    if !good {
        ERRORS.fetch_or(bit, Ordering::SeqCst);
    }
}
unsafe extern "C" fn buffer_switch(index: i32, direct: Bool) {
    SWITCHES.fetch_add(1, Ordering::SeqCst);
    callback_result(1, index == 1 && direct == Bool::TRUE);
}
unsafe extern "C" fn rate_changed(rate: f64) {
    callback_result(2, rate == 192000.25);
}
unsafe extern "C" fn message(
    selector: MessageSelector,
    value: i32,
    message: *mut c_void,
    opt: *mut f64,
) -> i32 {
    let good = selector.0 == 0x76543210 && value == -34567 && !message.is_null() && !opt.is_null();
    if !good {
        callback_result(4, false);
        return 0;
    }
    // 回调里只记录错误；断言放在 Rust 调用栈上，避免 panic 穿过 C ABI。
    let good = unsafe { message.cast::<u32>().read() == 0x10203040 && opt.read() == 3.25 };
    callback_result(4, good);
    unsafe {
        message.cast::<u32>().write(0x50607080);
        opt.write(6.5);
    }
    0x24681357
}
unsafe extern "C" fn time_switch(time: *mut Time, index: i32, direct: Bool) -> *mut Time {
    if time.is_null() {
        callback_result(8, false);
        return ptr::null_mut();
    }
    // pack(4) 中的 f64 以值读取，显式避免产生未对齐引用。
    let t = unsafe { time.read_unaligned() };
    let info = t.time_info;
    let code = t.time_code;
    let good = index == 1
        && direct == Bool::FALSE
        && info.speed == 1.25
        && info.system_time.hi == 0x12345678
        && info.system_time.lo == 0x9abcdef0
        && info.sample_position.hi == 0xfedcba98
        && info.sample_position.lo == 0x76543210
        && info.sample_rate == 192000.25
        && info.flags.bits() == 0x8000000f
        && code.speed == 0.75
        && code.time_code_samples.hi == 0x11223344
        && code.time_code_samples.lo == 0x55667788
        && code.flags.bits() == 0x80000103;
    callback_result(8, good);
    unsafe {
        (&raw mut (*time).time_info.sample_position.lo).write_unaligned(0x0badf00d);
        (&raw mut (*time).time_code.time_code_samples.hi).write_unaligned(0x13579bdf);
    }
    time
}
fn rust_callbacks() -> Callbacks {
    Callbacks {
        buffer_switch,
        sample_rate_did_change: rate_changed,
        asio_message: message,
        buffer_switch_time_info: Some(time_switch),
    }
}

fn compare_layout(probe: &Probe) {
    let lookup: unsafe extern "C" fn(*const i8) -> u64 =
        unsafe { probe.symbol(c"azo_probe_layout") };
    let mut checked = 0;
    let mut mismatches = Vec::new();
    let mut check = |key: &str, actual: usize| {
        let key_c = CString::new(key).unwrap();
        let expected = unsafe { lookup(key_c.as_ptr()) };
        if expected != actual as u64 {
            mismatches.push(format!("{key}: Rust={actual}, SDK={expected}"));
        }
        checked += 1;
    };
    macro_rules! layout {
        ($t:ty, $name:literal $(, $f:ident)*) => {{
            check(concat!($name, ".size"), size_of::<$t>());
            check(concat!($name, ".align"), align_of::<$t>());
            $(check(concat!($name, ".", stringify!($f)), offset_of!($t, $f));)*
        }};
    }
    check("pointer.size", size_of::<*mut c_void>());
    check("long.size", size_of::<std::ffi::c_long>());
    layout!(Samples, "Samples", hi, lo);
    layout!(TimeStamp, "TimeStamp");
    layout!(SampleRate, "SampleRate");
    layout!(Bool, "Bool");
    layout!(SampleType, "SampleType");
    layout!(ResultCode, "ResultCode");
    layout!(
        TimeCode,
        "TimeCode",
        speed,
        time_code_samples,
        flags,
        _placeholder
    );
    layout!(
        TimeInfo,
        "TimeInfo",
        speed,
        system_time,
        sample_position,
        sample_rate,
        flags,
        _reserved
    );
    layout!(Time, "Time", _reserved, time_info, time_code);
    layout!(
        Callbacks,
        "Callbacks",
        buffer_switch,
        sample_rate_did_change,
        asio_message,
        buffer_switch_time_info
    );
    layout!(
        ClockSource,
        "ClockSource",
        index,
        associated_channel,
        associated_group,
        is_current_source,
        name
    );
    layout!(
        ChannelInfo,
        "ChannelInfo",
        channel,
        is_input,
        is_active,
        channel_group,
        sample_type,
        name
    );
    layout!(BufferInfo, "BufferInfo", is_input, channel_num, buffers);
    layout!(
        InputMonitor,
        "InputMonitor",
        input,
        output,
        gain,
        state,
        pan
    );
    #[cfg(feature = "undocumented")]
    {
        layout!(
            ChannelControls,
            "ChannelControls",
            channel,
            is_input,
            gain,
            meter,
            _placeholder
        );
        layout!(
            TransportParameters,
            "TransportParameters",
            command,
            sample_position,
            track,
            track_switches,
            _placeholder
        );
    }
    layout!(IoFormatType, "IoFormatType");
    layout!(IoFormat, "IoFormat", format_type, _placeholder);
    layout!(
        InternalBufferInfo,
        "InternalBufferInfo",
        input_samples,
        output_samples
    );
    check("ResultCode.OK", ResultCode::OK.0 as usize);
    check("ResultCode.SUCCESS", ResultCode::SUCCESS.0 as usize);
    check("SampleType.PCM_F64_LSB", SampleType::PCM_F64_LSB.0 as usize);
    check(
        "FutureSelector.GET_IO_FORMAT",
        FutureSelector::GET_IO_FORMAT.0 as usize,
    );
    println!(
        "layout_comparisons={checked}; mismatches={}",
        mismatches.len()
    );
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    assert_eq!(size_of::<IIASIORedecl_Vtbl>(), 24 * size_of::<usize>());
}

fn callback_roundtrips(probe: &Probe) {
    unsafe {
        let exercise: unsafe extern "C" fn(*mut Callbacks) -> u32 =
            probe.symbol(c"azo_probe_exercise_callbacks");
        let cpp_callbacks: unsafe extern "C" fn(*mut Callbacks) =
            probe.symbol(c"azo_probe_cpp_callbacks");
        let cpp_status: unsafe extern "C" fn() -> u32 =
            probe.symbol(c"azo_probe_cpp_callback_status");
        let fill_time: unsafe extern "C" fn(*mut Time) = probe.symbol(c"azo_probe_fill_time");
        let mut rust = rust_callbacks();
        assert_eq!(exercise(&mut rust), 15);
        assert_eq!(SEEN.load(Ordering::SeqCst), 15);
        assert_eq!(ERRORS.load(Ordering::SeqCst), 0);
        let mut absent_time = rust_callbacks();
        absent_time.buffer_switch_time_info = None;
        let switches_before = SWITCHES.load(Ordering::SeqCst);
        assert_eq!(exercise(&mut absent_time), 0);
        assert_eq!(SWITCHES.load(Ordering::SeqCst), switches_before);
        let mut cpp = Callbacks::NOOP;
        cpp_callbacks(&mut cpp);
        let switch = cpp.buffer_switch;
        let rate = cpp.sample_rate_did_change;
        let message = cpp.asio_message;
        let time_callback = cpp.buffer_switch_time_info.unwrap();
        switch(1, Bool::TRUE);
        rate(192000.25);
        let mut payload: u32 = 0x10203040;
        let mut opt = 3.25;
        assert_eq!(
            message(
                MessageSelector(0x76543210),
                -34567,
                (&raw mut payload).cast(),
                &raw mut opt
            ),
            0x24681357
        );
        assert_eq!((payload, opt), (0x50607080, 6.5));
        #[repr(C, align(8))]
        struct TimeFrame {
            before: u32,
            time: Time,
            after: u32,
        }
        let mut frame = TimeFrame {
            before: 0xa5a5a5a5,
            time: Time::default(),
            after: 0x5a5a5a5a,
        };
        let time = &raw mut frame.time;
        assert_eq!(time as usize % 8, 4);
        fill_time(time);
        assert_eq!(time_callback(time, 1, Bool::FALSE), time);
        assert_eq!({ frame.time.time_info.sample_position.lo }, 0x0badf00d);
        assert_eq!({ frame.time.time_code.time_code_samples.hi }, 0x13579bdf);
        assert_eq!((frame.before, frame.after), (0xa5a5a5a5, 0x5a5a5a5a));
        assert_eq!(cpp_status(), 15);
        println!(
            "callbacks_cpp_to_rust=4/4; callbacks_rust_to_cpp=4/4; pointer_payload_return=passed; time_address_mod8=4_both_directions; nullable_time_callback=passed"
        );
    }
}

fn fake_roundtrip(probe: &Probe, error_width: usize) {
    unsafe {
        let create: unsafe extern "C" fn(u32) -> *mut c_void = probe.symbol(c"azo_probe_create");
        let iid: unsafe extern "C" fn(*mut GUID) = probe.symbol(c"azo_probe_iid");
        let seen: unsafe extern "C" fn(*mut c_void) -> u64 = probe.symbol(c"azo_probe_seen");
        let errors: unsafe extern "C" fn(*mut c_void) -> u32 = probe.symbol(c"azo_probe_errors");
        let callback_checks: unsafe extern "C" fn(*mut c_void) -> u32 =
            probe.symbol(c"azo_probe_callback_checks");
        let destroyed: unsafe extern "C" fn() -> u32 = probe.symbol(c"azo_probe_destroyed");
        let before = destroyed();
        let raw = create(error_width as u32);
        assert!(!raw.is_null());
        let driver = IIASIORedecl::from_raw(raw);
        let mut guid = GUID::zeroed();
        iid(&mut guid);
        let mut queried = ptr::null_mut();
        assert_eq!(driver.query(&guid, &mut queried).0, 0);
        assert_eq!(queried, raw);
        drop(IIASIORedecl::from_raw(queried));
        drop(driver.clone());
        let invalid_iid = GUID::from_u128(0x11111111_2222_3333_4444_555555555555);
        queried = raw;
        assert_eq!(
            driver.query(&invalid_iid, &mut queried).0,
            0x80004002u32 as i32
        );
        assert!(queried.is_null());
        assert_eq!(driver.init(0x12345678usize as *mut c_void), Bool::TRUE);
        let unknown_bool = driver.init(0x0badf00dusize as *mut c_void);
        assert_eq!(unknown_bool.0, 7);
        assert!(bool::try_from(unknown_bool).is_err());
        let mut name = [0xa5u8; 64];
        driver.get_driver_name(name.as_mut_ptr().add(16));
        assert_eq!(&name[..16], &[0xa5; 16]);
        assert_eq!(&name[16..47], &[b'N'; 31]);
        assert_eq!(name[47], 0);
        assert_eq!(&name[48..], &[0xa5; 16]);
        assert_eq!(driver.get_driver_version(), 0x12345678);
        // 物理分配保留 128 字节；124 边界 canary 在同一有效分配内观察，不触发越界 UB。
        let mut error = [0xa5u8; 160];
        driver.get_error_message(error.as_mut_ptr().add(16));
        assert_eq!(&error[..16], &[0xa5; 16]);
        assert!(error[16..16 + error_width - 1].iter().all(|&x| x == b'E'));
        assert_eq!(error[16 + error_width - 1], 0);
        assert!(error[16 + error_width..].iter().all(|&x| x == 0xa5));
        let crossed_124 = error[140..144].iter().any(|&x| x != 0xa5);
        assert_eq!(crossed_124, error_width == 128);
        println!(
            "error_bytes={error_width}; crossed_124_canary={crossed_124}; guard_after_128=intact"
        );
        let (mut input, mut output) = (0, 0);
        assert_eq!(driver.get_channels(&mut input, &mut output), ResultCode::OK);
        assert_eq!((input, output), (2, 3));
        assert_eq!(
            driver.get_latencies(&mut input, &mut output),
            ResultCode::OK
        );
        assert_eq!((input, output), (17, 29));
        let (mut min, mut max, mut preferred, mut step) = (0, 0, 0, 0);
        assert_eq!(
            driver.get_buffer_size(&mut min, &mut max, &mut preferred, &mut step),
            ResultCode::OK
        );
        assert_eq!((min, max, preferred, step), (8, 64, 16, -1));
        assert_eq!(driver.can_sample_rate(192000.25), ResultCode::OK);
        assert_eq!(driver.can_sample_rate(44100.0), ResultCode::NO_CLOCK);
        let mut rate = 0.0;
        assert_eq!(driver.get_sample_rate(&mut rate), ResultCode::OK);
        assert_eq!(rate, 48000.5);
        assert_eq!(driver.set_sample_rate(192000.25), ResultCode::OK);
        assert_eq!(driver.get_sample_rate(&mut rate), ResultCode::OK);
        assert_eq!(rate, 192000.25);
        let mut clock: ClockSource = std::mem::zeroed();
        let mut count = 1;
        assert_eq!(
            driver.get_clock_sources(&mut clock, &mut count),
            ResultCode::OK
        );
        assert_eq!(count, 1);
        assert_eq!(
            (
                clock.index,
                clock.associated_channel,
                clock.associated_group,
                clock.is_current_source
            ),
            (7, -1, -1, Bool::TRUE)
        );
        assert_eq!(&clock.name[..14], b"SDK fake clock");
        assert_eq!(driver.set_clock_source(7), ResultCode::OK);
        assert_eq!(driver.set_clock_source(8), ResultCode::INVALID_PARAMETER);
        let (mut samples, mut stamp) = (Samples::default(), TimeStamp::default());
        assert_eq!(
            driver.get_sample_position(&mut samples, &mut stamp),
            ResultCode::OK
        );
        assert_eq!(
            (samples.hi, samples.lo, stamp.hi, stamp.lo),
            (0xfedcba98, 0x76543210, 0x12345678, 0x9abcdef0)
        );
        let mut info: ChannelInfo = std::mem::zeroed();
        info.channel = 2;
        assert_eq!(driver.get_channel_info(&mut info), ResultCode::OK);
        assert_eq!(
            (info.is_active, info.channel_group, info.sample_type.0),
            (Bool::TRUE, 9, 0x76543210)
        );
        assert_eq!(&info.name[..31], &[b'C'; 31]);
        assert_eq!(driver.start(), ResultCode::INVALID_MODE);
        let mut buffers = BufferInfo {
            is_input: Bool::FALSE,
            channel_num: 2,
            buffers: [ptr::null_mut(); 2],
        };
        let mut callbacks = rust_callbacks();
        assert_eq!(
            driver.create_buffers(&mut buffers, 1, 8, &mut callbacks),
            ResultCode::OK
        );
        let pointers = buffers.buffers;
        assert!(!pointers[0].is_null() && !pointers[1].is_null());
        assert_ne!(pointers[0], pointers[1]);
        pointers[0].cast::<f64>().write(12.25);
        pointers[1].cast::<f64>().write(-23.5);
        assert_eq!(pointers[0].cast::<f64>().read(), 12.25);
        assert_eq!(pointers[1].cast::<f64>().read(), -23.5);
        let switches = SWITCHES.load(Ordering::SeqCst);
        assert_eq!(driver.start(), ResultCode::OK);
        assert_eq!(SWITCHES.load(Ordering::SeqCst), switches + 1);
        assert_eq!(callback_checks(raw), 15);
        assert_eq!(driver.output_ready(), ResultCode::SUCCESS);
        assert_eq!(driver.stop(), ResultCode::OK);
        assert_eq!(SWITCHES.load(Ordering::SeqCst), switches + 2);
        assert_eq!(driver.dispose_buffers(), ResultCode::OK);
        assert_eq!(driver.control_panel(), ResultCode::NOT_PRESENT);
        let mut future_payload: i32 = 0;
        assert_eq!(
            driver
                .future(FutureSelector(0x76543210), (&raw mut future_payload).cast())
                .0,
            -1234567
        );
        assert_eq!(future_payload, -123456789);
        assert_eq!(
            driver.future(FutureSelector(-1), ptr::null_mut()).0,
            0x76543210
        );
        assert_eq!(
            seen(raw),
            0x00ff_ffff,
            "all 24 SDK virtual slots must be reached"
        );
        assert_eq!(errors(raw), 0);
        assert_eq!(ERRORS.load(Ordering::SeqCst), 0);
        drop(driver);
        assert_eq!(
            destroyed(),
            before + 1,
            "final COM Release destroys exactly once"
        );
        println!(
            "iasio_slots=24/24; unknown_raw_values=preserved; synchronous_start_and_last_stop_callback=passed; final_release=passed"
        );
    }
}

#[test]
#[ignore = "requires local SDK probe; set AZO_SDK_ABI_DLL and run --ignored --nocapture"]
fn sdk_layout_vtable_callbacks_and_error_canaries() {
    let probe = Probe::load();
    compare_layout(&probe);
    callback_roundtrips(&probe);
    fake_roundtrip(&probe, 124);
    fake_roundtrip(&probe, 128);
}
