use std::cell::{Cell, RefCell, UnsafeCell};
use std::ffi::c_void;
use std::ptr;
use std::rc::Rc;

use windows_core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};

use crate::driver::{Owner, Phase};
use crate::dto::ChannelId;
use crate::future::{CanDoIoFormat, GetIoFormat, SetIoFormat};
use crate::{Error, sys};

const CHANNEL: ChannelId = ChannelId {
    input: false,
    index: 0,
};

thread_local! {
    static CALLBACK_COUNT: Cell<usize> = const { Cell::new(0) };
    static BAD_CALLBACK: Cell<bool> = const { Cell::new(false) };
}

// 回调只引用静态代码及当前测试线程的计数器，不借用 Owner 或栈上上下文。
unsafe extern "C" fn buffer_switch(index: i32, direct: sys::Bool) {
    CALLBACK_COUNT.with(|count| count.set(count.get() + 1));
    if index != 1 || direct != sys::Bool::TRUE {
        BAD_CALLBACK.with(|bad| bad.set(true));
    }
}

fn callback_count() -> usize {
    CALLBACK_COUNT.with(Cell::get)
}

fn callbacks() -> sys::Callbacks {
    sys::Callbacks {
        buffer_switch,
        buffer_switch_time_info: None,
        ..sys::Callbacks::NOOP
    }
}

#[derive(Clone, Copy)]
enum BufferFault {
    None,
    Null,
    WrongDirection,
    InvalidBoolean,
    WrongIndex,
}

#[derive(Clone)]
struct Behavior {
    init: sys::Bool,
    query_code: sys::ResultCode,
    write_outputs: bool,
    terminated: bool,
    counts: (i32, i32),
    latencies: (i32, i32),
    sizes: (i32, i32, i32, i32),
    rate: f64,
    clock_count: i32,
    clock_current: sys::Bool,
    duplicate_clocks: bool,
    channel_index: i32,
    channel_input: sys::Bool,
    channel_active: sys::Bool,
    channel_group: i32,
    buffer_fault: BufferFault,
    create_code: sys::ResultCode,
    start_code: sys::ResultCode,
    stop_code: sys::ResultCode,
    future_code: sys::ResultCode,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            init: sys::Bool::TRUE,
            query_code: sys::ResultCode::OK,
            write_outputs: true,
            terminated: true,
            counts: (0, 2),
            latencies: (0, 64),
            sizes: (16, 256, 64, -1),
            rate: 48_000.0,
            clock_count: 1,
            clock_current: sys::Bool::TRUE,
            duplicate_clocks: false,
            channel_index: 0,
            channel_input: sys::Bool::FALSE,
            channel_active: sys::Bool::TRUE,
            channel_group: 0,
            buffer_fault: BufferFault::None,
            create_code: sys::ResultCode::OK,
            start_code: sys::ResultCode::OK,
            stop_code: sys::ResultCode::OK,
            future_code: sys::ResultCode::SUCCESS,
        }
    }
}

struct Probe {
    behavior: RefCell<Behavior>,
    events: RefCell<Vec<&'static str>>,
    releases: Cell<usize>,
    destroyed: Cell<bool>,
    disposes: Cell<usize>,
    infos: Cell<*mut sys::BufferInfo>,
    callbacks: Cell<*mut sys::Callbacks>,
    capacities: RefCell<Vec<i32>>,
    audio: UnsafeCell<[u8; 16]>,
}

impl Probe {
    fn event(&self, event: &'static str) {
        self.events.borrow_mut().push(event);
    }

    // Fake 没有后台线程；晚回调由测试显式触发，覆盖 Owner 已析构的情形。
    fn fire(&self, event: &'static str) {
        self.event(event);
        let table = unsafe { self.callbacks.get().read_unaligned() };
        unsafe { (table.buffer_switch)(1, sys::Bool::TRUE) };
    }
}

// 手写合成 COM 对象只有初始引用；测试不额外 AddRef 来掩盖过早 Release。
#[repr(C)]
struct Fake {
    vtable: *const sys::IIASIORedecl_Vtbl,
    references: Cell<u32>,
    probe: Rc<Probe>,
}

fn fake_owner() -> (Owner, Rc<Probe>) {
    let probe = Rc::new(Probe {
        behavior: RefCell::new(Behavior::default()),
        events: RefCell::new(Vec::new()),
        releases: Cell::new(0),
        destroyed: Cell::new(false),
        disposes: Cell::new(0),
        infos: Cell::new(ptr::null_mut()),
        callbacks: Cell::new(ptr::null_mut()),
        capacities: RefCell::new(Vec::new()),
        audio: UnsafeCell::new([0; 16]),
    });
    let fake = Box::new(Fake {
        vtable: &VTABLE,
        references: Cell::new(1),
        probe: probe.clone(),
    });
    let raw = unsafe { sys::IIASIORedecl::from_raw(Box::into_raw(fake).cast()) };
    (unsafe { Owner::from_raw(raw) }, probe)
}

fn initialized_owner() -> (Owner, Rc<Probe>) {
    let (mut owner, probe) = fake_owner();
    owner.init(None).unwrap();
    (owner, probe)
}

unsafe fn probe(this: *mut c_void) -> Rc<Probe> {
    unsafe { &*this.cast::<Fake>() }.probe.clone()
}

unsafe extern "system" fn query_interface(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || out.is_null() {
        return HRESULT(0x8000_4003_u32 as i32);
    }
    unsafe { *out = ptr::null_mut() };
    if unsafe { *iid } == IUnknown::IID {
        unsafe {
            add_ref(this);
            *out = this;
        }
        HRESULT(0)
    } else {
        HRESULT(0x8000_4002_u32 as i32)
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let fake = unsafe { &*this.cast::<Fake>() };
    let count = fake.references.get() + 1;
    fake.references.set(count);
    count
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let fake = unsafe { &*this.cast::<Fake>() };
    let count = fake.references.get() - 1;
    fake.references.set(count);
    fake.probe.event("release");
    fake.probe.releases.set(fake.probe.releases.get() + 1);
    if count == 0 {
        fake.probe.destroyed.set(true);
        unsafe { drop(Box::from_raw(this.cast::<Fake>())) };
    }
    count
}

unsafe extern "system" fn init(this: *mut c_void, _: *mut c_void) -> sys::Bool {
    let probe = unsafe { probe(this) };
    probe.event("init");
    probe.behavior.borrow().init
}

unsafe fn write_string(out: *mut u8, size: usize, terminated: bool) {
    unsafe {
        ptr::write_bytes(out, b'x', size);
        if terminated {
            *out.add(size - 1) = 0;
        }
    }
}

unsafe extern "system" fn name(this: *mut c_void, out: *mut u8) {
    let probe = unsafe { probe(this) };
    unsafe { write_string(out, 32, probe.behavior.borrow().terminated) };
}

unsafe extern "system" fn version(_: *mut c_void) -> i32 {
    2
}

unsafe extern "system" fn last_error(this: *mut c_void, out: *mut u8) {
    let probe = unsafe { probe(this) };
    // 故意写满 128 字节，回归保护曾误用 124 字节容量的调用方。
    unsafe { write_string(out, 128, probe.behavior.borrow().terminated) };
}

unsafe extern "system" fn start(this: *mut c_void) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    probe.event("start.enter");
    probe.fire("start.callback");
    probe.event("start.return");
    probe.behavior.borrow().start_code
}

unsafe extern "system" fn stop(this: *mut c_void) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    probe.event("stop.enter");
    probe.fire("stop.callback");
    probe.event("stop.return");
    probe.behavior.borrow().stop_code
}

unsafe extern "system" fn channels(
    this: *mut c_void,
    input: *mut i32,
    output: *mut i32,
) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    probe.event("channels");
    let behavior = probe.behavior.borrow();
    if behavior.write_outputs {
        unsafe {
            *input = behavior.counts.0;
            *output = behavior.counts.1;
        }
    }
    behavior.query_code
}

unsafe extern "system" fn latencies(
    this: *mut c_void,
    input: *mut i32,
    output: *mut i32,
) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    let behavior = probe.behavior.borrow();
    if behavior.write_outputs {
        unsafe {
            *input = behavior.latencies.0;
            *output = behavior.latencies.1;
        }
    }
    behavior.query_code
}

unsafe extern "system" fn buffer_size(
    this: *mut c_void,
    min: *mut i32,
    max: *mut i32,
    preferred: *mut i32,
    granularity: *mut i32,
) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    let behavior = probe.behavior.borrow();
    if behavior.write_outputs {
        unsafe {
            *min = behavior.sizes.0;
            *max = behavior.sizes.1;
            *preferred = behavior.sizes.2;
            *granularity = behavior.sizes.3;
        }
    }
    behavior.query_code
}

unsafe extern "system" fn can_rate(this: *mut c_void, _: f64) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    probe.event("can_rate");
    probe.behavior.borrow().query_code
}

unsafe extern "system" fn get_rate(this: *mut c_void, out: *mut f64) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    let behavior = probe.behavior.borrow();
    if behavior.write_outputs {
        unsafe { *out = behavior.rate }
    }
    behavior.query_code
}

unsafe extern "system" fn set_rate(this: *mut c_void, rate: f64) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    probe.event("set_rate");
    probe.behavior.borrow_mut().rate = rate;
    probe.behavior.borrow().query_code
}

unsafe extern "system" fn clocks(
    this: *mut c_void,
    out: *mut sys::ClockSource,
    count: *mut i32,
) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    let capacity = unsafe { *count };
    probe.capacities.borrow_mut().push(capacity);
    let behavior = probe.behavior.borrow();
    if behavior.write_outputs {
        // 非法数量只通过返回值报告，fake 自身绝不越过调用方容量写入。
        for index in 0..capacity.min(behavior.clock_count).max(0) {
            let mut name = [b'c'; 32];
            if behavior.terminated {
                name[31] = 0;
            }
            unsafe {
                out.add(index as usize).write_unaligned(sys::ClockSource {
                    index: if behavior.duplicate_clocks { 0 } else { index },
                    associated_channel: -1,
                    associated_group: -1,
                    is_current_source: behavior.clock_current,
                    name,
                })
            };
        }
        unsafe { *count = behavior.clock_count };
    }
    behavior.query_code
}

unsafe extern "system" fn set_clock(_: *mut c_void, _: i32) -> sys::ResultCode {
    sys::ResultCode::NOT_PRESENT
}

unsafe extern "system" fn position(
    _: *mut c_void,
    _: *mut sys::Samples,
    _: *mut sys::TimeStamp,
) -> sys::ResultCode {
    sys::ResultCode::NOT_PRESENT
}

unsafe extern "system" fn channel_info(
    this: *mut c_void,
    out: *mut sys::ChannelInfo,
) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    let behavior = probe.behavior.borrow();
    if behavior.write_outputs {
        let mut name = [b'n'; 32];
        if behavior.terminated {
            name[31] = 0;
        }
        unsafe {
            out.write_unaligned(sys::ChannelInfo {
                channel: behavior.channel_index,
                is_input: behavior.channel_input,
                is_active: behavior.channel_active,
                channel_group: behavior.channel_group,
                sample_type: sys::SampleType::PCM_I32_LSB,
                name,
            })
        };
    }
    behavior.query_code
}

unsafe extern "system" fn create_buffers(
    this: *mut c_void,
    infos: *mut sys::BufferInfo,
    count: i32,
    _: i32,
    callbacks: *mut sys::Callbacks,
) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    probe.event("create.enter");
    probe.infos.set(infos);
    probe.callbacks.set(callbacks);
    let behavior = probe.behavior.borrow().clone();
    for index in 0..count {
        let mut info = unsafe { infos.add(index as usize).read_unaligned() };
        let audio = probe.audio.get().cast::<u8>();
        info.buffers = [audio.cast(), unsafe { audio.add(8) }.cast()];
        match behavior.buffer_fault {
            BufferFault::None => {}
            BufferFault::Null => info.buffers[1] = ptr::null_mut(),
            BufferFault::WrongDirection => info.is_input = sys::Bool::TRUE,
            BufferFault::InvalidBoolean => info.is_input = sys::Bool(7),
            BufferFault::WrongIndex => info.channel_num = 9,
        }
        unsafe { infos.add(index as usize).write_unaligned(info) };
    }
    probe.fire("create.callback");
    probe.event("create.return");
    behavior.create_code
}

unsafe extern "system" fn dispose(this: *mut c_void) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    probe.disposes.set(probe.disposes.get() + 1);
    sys::ResultCode::OK
}

unsafe extern "system" fn control_panel(_: *mut c_void) -> sys::ResultCode {
    sys::ResultCode::NOT_PRESENT
}

unsafe extern "system" fn future(
    this: *mut c_void,
    _: sys::FutureSelector,
    _: *mut c_void,
) -> sys::ResultCode {
    let probe = unsafe { probe(this) };
    probe.event("future");
    probe.behavior.borrow().future_code
}

unsafe extern "system" fn output_ready(_: *mut c_void) -> sys::ResultCode {
    sys::ResultCode::NOT_PRESENT
}

static VTABLE: sys::IIASIORedecl_Vtbl = sys::IIASIORedecl_Vtbl {
    base__: IUnknown_Vtbl {
        QueryInterface: query_interface,
        AddRef: add_ref,
        Release: release,
    },
    init,
    get_driver_name: name,
    get_driver_version: version,
    get_error_message: last_error,
    start,
    stop,
    get_channels: channels,
    get_latencies: latencies,
    get_buffer_size: buffer_size,
    can_sample_rate: can_rate,
    get_sample_rate: get_rate,
    set_sample_rate: set_rate,
    get_clock_sources: clocks,
    set_clock_source: set_clock,
    get_sample_position: position,
    get_channel_info: channel_info,
    create_buffers,
    dispose_buffers: dispose,
    control_panel,
    future,
    output_ready,
};

fn io_format() -> sys::IoFormat {
    sys::IoFormat {
        format_type: sys::IoFormatType::PCM,
        _placeholder: [0; 508],
    }
}

#[test]
fn unregistered_owner_releases_exactly_once() {
    for initialize in [false, true] {
        let (mut owner, probe) = fake_owner();
        if initialize {
            owner.init(None).unwrap();
        }
        let raw = unsafe { owner.as_raw() };
        assert!(!Interface::as_raw(raw).is_null());
        drop(owner);
        assert_eq!(probe.releases.get(), 1);
        assert!(probe.destroyed.get());
        assert_eq!(probe.disposes.get(), 0);
    }
}

#[test]
fn synchronous_callbacks_and_late_callback_keep_fixed_registration_alive() {
    let (mut owner, probe) = initialized_owner();
    let before = callback_count();
    unsafe { owner.create_buffers(&[CHANNEL], 64, callbacks()) }.unwrap();
    assert!(owner.registration_started());
    assert!(!owner.stop_returned());
    assert_eq!(owner.phase(), Phase::Registered);
    let infos = owner.buffer_infos().unwrap().as_ptr();
    let callback_table = probe.callbacks.get();
    assert_eq!(infos, probe.infos.get().cast_const());
    // 移动 owner 不移动传给驱动的分配。
    let mut moved = Box::new(owner);
    moved.channel_counts().unwrap();
    moved.start().unwrap();
    assert_eq!(moved.phase(), Phase::Running);
    moved.stop().unwrap();
    assert!(moved.stop_returned());
    assert_eq!(moved.phase(), Phase::StopReturned);
    drop(moved);
    assert_eq!(probe.releases.get(), 0);
    assert_eq!(probe.disposes.get(), 0);
    assert!(!probe.destroyed.get());
    assert_eq!(callback_table, probe.callbacks.get());
    let retained = unsafe { infos.read_unaligned() };
    let buffers = retained.buffers;
    assert!(buffers.iter().all(|buffer| !buffer.is_null()));
    probe.fire("late.callback");
    assert_eq!(callback_count() - before, 4);
    assert!(!BAD_CALLBACK.with(Cell::get));
    assert_eq!(
        &*probe.events.borrow(),
        &[
            "init",
            "create.enter",
            "create.callback",
            "create.return",
            "channels",
            "start.enter",
            "start.callback",
            "start.return",
            "stop.enter",
            "stop.callback",
            "stop.return",
            "late.callback",
        ]
    );
}

#[test]
fn partial_create_failure_retains_driver_and_registration() {
    let (mut owner, probe) = initialized_owner();
    probe.behavior.borrow_mut().create_code = sys::ResultCode::HW_MALFUNCTION;
    let before = callback_count();
    let error = unsafe { owner.create_buffers(&[CHANNEL], 64, callbacks()) }.unwrap_err();
    assert_eq!(error.code(), Some(sys::ResultCode::HW_MALFUNCTION));
    assert!(owner.registration_started());
    assert!(owner.buffer_infos().is_none());
    assert_eq!(owner.phase(), Phase::Failed);
    assert!(owner.start().is_err());
    owner.stop().unwrap();
    drop(owner);
    probe.fire("late.callback");
    assert_eq!(callback_count() - before, 3);
    assert_eq!(probe.releases.get(), 0);
    assert_eq!(probe.disposes.get(), 0);
}

#[test]
fn prepared_and_failed_start_both_allow_stop() {
    for fail_start in [false, true] {
        let (mut owner, probe) = initialized_owner();
        unsafe { owner.create_buffers(&[CHANNEL], 64, callbacks()) }.unwrap();
        if fail_start {
            probe.behavior.borrow_mut().start_code = sys::ResultCode::HW_MALFUNCTION;
            assert!(owner.start().is_err());
            assert_eq!(owner.phase(), Phase::Failed);
        }
        owner.stop().unwrap();
        assert!(owner.stop_returned());
        assert!(owner.start().is_err());
        assert!(owner.stop().is_err());
        drop(owner);
        assert_eq!(probe.releases.get(), 0);
        assert_eq!(probe.disposes.get(), 0);
    }
}

#[test]
fn failed_stop_reports_return_without_claiming_success_or_releasing() {
    let (mut owner, probe) = initialized_owner();
    unsafe { owner.create_buffers(&[CHANNEL], 64, callbacks()) }.unwrap();
    owner.start().unwrap();
    probe.behavior.borrow_mut().stop_code = sys::ResultCode::HW_MALFUNCTION;
    assert!(owner.stop().is_err());
    assert!(owner.stop_returned());
    assert_eq!(owner.phase(), Phase::Failed);
    drop(owner);
    probe.fire("late.callback");
    assert_eq!(probe.releases.get(), 0);
    assert_eq!(probe.disposes.get(), 0);
}

#[test]
fn drop_does_not_implicitly_stop_a_running_registration() {
    let (mut owner, probe) = initialized_owner();
    unsafe { owner.create_buffers(&[CHANNEL], 64, callbacks()) }.unwrap();
    owner.start().unwrap();
    drop(owner);
    assert!(!probe.events.borrow().contains(&"stop.enter"));
    assert_eq!(probe.disposes.get(), 0);
    assert_eq!(probe.releases.get(), 0);
    probe.fire("late.callback");
}

#[test]
fn invalid_buffer_metadata_is_not_exposed_but_is_retained() {
    for fault in [
        BufferFault::Null,
        BufferFault::WrongDirection,
        BufferFault::InvalidBoolean,
        BufferFault::WrongIndex,
    ] {
        let (mut owner, probe) = initialized_owner();
        probe.behavior.borrow_mut().buffer_fault = fault;
        assert!(unsafe { owner.create_buffers(&[CHANNEL], 64, callbacks()) }.is_err());
        assert!(owner.registration_started());
        assert!(owner.buffer_infos().is_none());
        drop(owner);
        probe.fire("late.callback");
        assert_eq!(probe.releases.get(), 0);
        assert_eq!(probe.disposes.get(), 0);
    }
}

#[test]
fn invalid_registration_arguments_do_not_enter_driver_or_retain_it() {
    let (mut owner, probe) = initialized_owner();
    for (channels, frames) in [
        (vec![], 64),
        (vec![CHANNEL], 0),
        (vec![CHANNEL, CHANNEL], 64),
        (
            vec![ChannelId {
                index: -1,
                ..CHANNEL
            }],
            64,
        ),
    ] {
        assert!(unsafe { owner.create_buffers(&channels, frames, callbacks()) }.is_err());
        assert!(!owner.registration_started());
        assert_eq!(owner.phase(), Phase::Initialized);
    }
    assert!(!probe.events.borrow().contains(&"create.enter"));
    drop(owner);
    assert_eq!(probe.releases.get(), 1);
}

#[test]
fn io_format_zero_and_unknown_codes_remain_errors() {
    let (mut owner, probe) = initialized_owner();
    for code in [
        sys::ResultCode::OK,
        sys::ResultCode::INVALID_MODE,
        sys::ResultCode(37),
    ] {
        probe.behavior.borrow_mut().future_code = code;
        assert_eq!(
            owner
                .future::<GetIoFormat>(&mut io_format())
                .unwrap_err()
                .code(),
            Some(code)
        );
        assert_eq!(
            owner
                .future::<CanDoIoFormat>(&mut io_format())
                .unwrap_err()
                .code(),
            Some(code)
        );
        assert_eq!(
            owner
                .future::<SetIoFormat>(&mut io_format())
                .unwrap_err()
                .code(),
            Some(code)
        );
    }
    probe.behavior.borrow_mut().future_code = sys::ResultCode::SUCCESS;
    owner.future::<SetIoFormat>(&mut io_format()).unwrap();
    probe.behavior.borrow_mut().query_code = sys::ResultCode::SUCCESS;
    assert_eq!(
        owner.channel_counts().unwrap_err().code(),
        Some(sys::ResultCode::SUCCESS)
    );
}

#[test]
fn registered_queries_remain_available_but_configuration_is_closed() {
    let (mut owner, probe) = initialized_owner();
    unsafe { owner.create_buffers(&[CHANNEL], 64, callbacks()) }.unwrap();
    for phase in [Phase::Registered, Phase::Running, Phase::StopReturned] {
        match phase {
            Phase::Running => owner.start().unwrap(),
            Phase::StopReturned => owner.stop().unwrap(),
            _ => {}
        }
        owner.channel_counts().unwrap();
        owner.latencies().unwrap();
        owner.buffer_size().unwrap();
        owner.get_sample_rate().unwrap();
        owner.clock_sources(1).unwrap();
        owner.channel_info(CHANNEL).unwrap();
        owner.future::<GetIoFormat>(&mut io_format()).unwrap();
        owner.future::<CanDoIoFormat>(&mut io_format()).unwrap();
        let entries = probe.events.borrow().len();
        assert!(owner.future::<SetIoFormat>(&mut io_format()).is_err());
        assert!(owner.set_sample_rate(96_000.0).is_err());
        assert_eq!(probe.events.borrow().len(), entries);
    }
}

#[test]
fn strings_are_bounded_and_error_message_uses_all_128_bytes() {
    let (owner, probe) = initialized_owner();
    assert_eq!(owner.name().unwrap().as_bytes().len(), 31);
    assert_eq!(owner.last_error().unwrap().as_bytes().len(), 127);
    assert_eq!(owner.version().unwrap(), 2);
    probe.behavior.borrow_mut().terminated = false;
    assert!(matches!(
        owner.name(),
        Err(Error::UnterminatedString { .. })
    ));
    assert!(matches!(
        owner.last_error(),
        Err(Error::UnterminatedString { .. })
    ));
    assert!(owner.channel_info(CHANNEL).is_err());
    assert!(owner.clock_sources(1).is_err());
}

#[test]
fn invalid_init_bool_and_rejection_do_not_register() {
    for value in [sys::Bool::FALSE, sys::Bool(2)] {
        let (mut owner, probe) = fake_owner();
        probe.behavior.borrow_mut().init = value;
        assert!(owner.init(None).is_err());
        assert_eq!(owner.phase(), Phase::Failed);
        assert!(!owner.registration_started());
        assert!(owner.channel_counts().is_err());
        assert!(owner.stop().is_err());
        drop(owner);
        assert_eq!(probe.releases.get(), 1);
    }
}

#[test]
fn clock_capacity_and_initialized_storage_bound_driver_output() {
    let (owner, probe) = initialized_owner();
    assert!(owner.clock_sources(0).is_err());
    assert!(owner.clock_sources(usize::MAX).is_err());
    assert!(probe.capacities.borrow().is_empty());
    for reported in [-1, 3] {
        probe.behavior.borrow_mut().clock_count = reported;
        assert!(
            matches!(owner.clock_sources(2), Err(Error::CapacityExceeded { capacity: 2, reported: value, .. }) if value == reported)
        );
    }
    assert_eq!(&*probe.capacities.borrow(), &[2, 2]);
    probe.behavior.borrow_mut().clock_count = 0;
    assert!(owner.clock_sources(2).unwrap().is_empty());
    probe.behavior.borrow_mut().clock_count = 2;
    probe.behavior.borrow_mut().duplicate_clocks = true;
    assert!(owner.clock_sources(2).is_err());
    probe.behavior.borrow_mut().duplicate_clocks = false;
    probe.behavior.borrow_mut().clock_current = sys::Bool(2);
    assert!(owner.clock_sources(2).is_err());
    probe.behavior.borrow_mut().write_outputs = false;
    assert!(owner.clock_sources(2).is_err());
}

#[test]
fn omitted_or_invalid_scalar_query_outputs_are_rejected() {
    let (mut owner, probe) = initialized_owner();
    probe.behavior.borrow_mut().write_outputs = false;
    assert!(owner.channel_counts().is_err());
    assert!(owner.latencies().is_err());
    assert!(owner.buffer_size().is_err());
    assert!(owner.get_sample_rate().is_err());
    assert!(owner.channel_info(CHANNEL).is_err());
    probe.behavior.borrow_mut().write_outputs = true;
    probe.behavior.borrow_mut().counts = (-1, 2);
    probe.behavior.borrow_mut().latencies = (0, -1);
    assert!(owner.channel_counts().is_err());
    assert!(owner.latencies().is_err());
    for sizes in [
        (0, 256, 64, -1),
        (64, 16, 32, 0),
        (16, 256, 512, -1),
        (16, 256, 64, -2),
        (16, 256, 63, -1),
        (16, 256, 65, 16),
    ] {
        probe.behavior.borrow_mut().sizes = sizes;
        assert!(owner.buffer_size().is_err());
    }
    for rate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0] {
        probe.behavior.borrow_mut().rate = rate;
        assert!(owner.get_sample_rate().is_err());
        assert!(owner.can_sample_rate(rate).is_err());
        if rate != 0.0 {
            assert!(owner.set_sample_rate(rate).is_err());
        }
    }
    owner.set_sample_rate(0.0).unwrap();
}

#[test]
fn channel_identity_boolean_and_group_are_validated() {
    let (owner, probe) = initialized_owner();
    for input in [sys::Bool::TRUE, sys::Bool(2)] {
        probe.behavior.borrow_mut().channel_input = input;
        assert!(owner.channel_info(CHANNEL).is_err());
    }
    probe.behavior.borrow_mut().channel_input = sys::Bool::FALSE;
    probe.behavior.borrow_mut().channel_index = 1;
    assert!(owner.channel_info(CHANNEL).is_err());
    probe.behavior.borrow_mut().channel_index = 0;
    probe.behavior.borrow_mut().channel_active = sys::Bool(2);
    assert!(owner.channel_info(CHANNEL).is_err());
    probe.behavior.borrow_mut().channel_active = sys::Bool::TRUE;
    probe.behavior.borrow_mut().channel_group = -1;
    assert!(owner.channel_info(CHANNEL).is_err());
}
