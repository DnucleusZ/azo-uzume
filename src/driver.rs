use std::ffi::CString;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::num::NonZeroI32;
use std::ptr::{self, NonNull};
use std::rc::Rc;

use windows_core::{GUID, HSTRING, IUnknown};

use crate::dto::{self, ChannelId, Granularity};
use crate::future::AsioFuture;
use crate::utils::{
    COMInit, cast_decoupled, check_code, check_future_code, parse_bool, parse_string,
};
use crate::win::{CLSCTX_INPROC_SERVER, CoCreateInstance, HWND};
use crate::{Error, Result, WinResult, sys};

/// Metadata of an ASIO driver registered in the system.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Metadata {
    pub clsid: GUID,
    pub description: HSTRING,
}

impl Metadata {
    /// Enumerate registered ASIO drivers, skipping malformed registry entries.
    pub fn enumerate() -> WinResult<Vec<Self>> {
        let software_key = windows_registry::LOCAL_MACHINE.open("SOFTWARE\\ASIO")?;
        Ok(software_key
            .keys()?
            .filter_map(|name| {
                let key = software_key.open(&name).ok()?;
                Self::from_registry(&key).ok()
            })
            .collect())
    }

    fn from_registry(key: &windows_registry::Key) -> WinResult<Self> {
        let clsid = key
            .get_string("clsid")?
            .trim_matches(['{', '}'])
            .try_into()?;
        let description = key.get_hstring("description")?;
        Ok(Self { clsid, description })
    }
}

/// Control-call facts, not a statement about callback quiescence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Loaded,
    Initialized,
    /// create_buffers returned OK and its output metadata passed validation.
    Registered,
    Running,
    /// stop returned OK. Callbacks may still be running or arrive later.
    StopReturned,
    /// An initialization, registration, start, or stop attempt failed.
    Failed,
}

// 字段析构顺序是合同的一部分：未注册时先 Release，再配对 CoUninitialize。
struct Inner {
    raw: sys::IIASIORedecl,
    _com: Option<COMInit>,
}

struct Registration {
    // 分配在暴露指针前已由 Box::into_raw 消费。之后只移动 raw 指针，
    // 不再移动 Box 的唯一所有者，也不重建 Box 或借用为 &mut。
    infos: NonNull<[sys::BufferInfo]>,
    callbacks: NonNull<sys::Callbacks>,
}

/// Exclusive, owner-thread ASIO control. This type is !Send, !Sync and !Clone.
///
/// Before registration, dropping the owner releases its interface and then its
/// COM initialization. Once create_buffers has been entered, all registration
/// allocations, the driver reference and COM initialization are retained until
/// process exit, even if create/start/stop fails. Drop then calls no driver code.
///
/// Queries and explicit controls are synchronous third-party calls and have no
/// time bound. Hard deadlines require process supervision by the host.
///
/// ~~~compile_fail
/// fn requires_send<T: Send>() {}
/// requires_send::<azo::driver::Owner>();
/// ~~~
/// ~~~compile_fail
/// fn requires_sync<T: Sync>() {}
/// requires_sync::<azo::driver::Owner>();
/// ~~~
/// ~~~compile_fail
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<azo::driver::Owner>();
/// ~~~
pub struct Owner {
    inner: ManuallyDrop<Inner>,
    registration: Option<Registration>,
    phase: Phase,
    stop_returned: bool,
    buffers_valid: bool,
    _owner_thread: PhantomData<Rc<()>>,
}

impl Owner {
    pub fn new(clsid: &GUID) -> Result<Self> {
        let com = COMInit::new()?;
        // IASIO reuses its class ID as its interface ID. Only in-process drivers
        // are eligible; the ASIO ABI does not support COM marshalling.
        let unknown: IUnknown =
            unsafe { CoCreateInstance(clsid, None, CLSCTX_INPROC_SERVER as _) }?;
        let raw = unsafe { cast_decoupled::<sys::IIASIORedecl>(&unknown, clsid) }?;
        Ok(Self::with_inner(Inner {
            raw,
            _com: Some(com),
        }))
    }

    /// Transfer one owned interface reference, without initializing COM.
    ///
    /// # Safety
    /// The interface must implement this ABI, obey all ASIO output capacities
    /// and keep all writes within them. No competing owner may control it.
    /// The caller must keep its COM apartment and driver implementation alive
    /// until this owner is dropped, or until process exit if registration is
    /// attempted. A fake interface may omit COM only if it needs no apartment.
    pub unsafe fn from_raw(raw: sys::IIASIORedecl) -> Self {
        Self::with_inner(Inner { raw, _com: None })
    }

    fn with_inner(inner: Inner) -> Self {
        Self {
            inner: ManuallyDrop::new(inner),
            registration: None,
            phase: Phase::Loaded,
            stop_returned: false,
            buffers_valid: false,
            _owner_thread: PhantomData,
        }
    }

    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// True as soon as the driver's create_buffers entry point is entered.
    pub fn registration_started(&self) -> bool {
        self.registration.is_some()
    }

    /// True after an actual stop call returns, including an error return.
    /// This is neither a success indicator nor proof of callback quiescence.
    pub const fn stop_returned(&self) -> bool {
        self.stop_returned
    }

    pub fn init(&mut self, window_handle: Option<HWND>) -> Result<()> {
        self.require("init", self.phase == Phase::Loaded)?;
        self.phase = Phase::Failed;
        let accepted = unsafe { self.inner.raw.init(window_handle.unwrap_or_default().0) };
        if !parse_bool("init", accepted)? {
            return Err(Error::InitializationRejected);
        }
        self.phase = Phase::Initialized;
        Ok(())
    }

    pub fn name(&self) -> Result<CString> {
        let mut bytes = [0xff; 32];
        unsafe { self.inner.raw.get_driver_name(bytes.as_mut_ptr()) };
        parse_string("driver.name", &bytes)
    }

    pub fn version(&self) -> Result<sys::DriverVersion> {
        Ok(unsafe { self.inner.raw.get_driver_version() })
    }

    /// IASIO getErrorMessage takes 128 bytes. The 124-byte ASIODriverInfo
    /// member is a separate wrapper structure, not this method's capacity.
    pub fn last_error(&self) -> Result<CString> {
        let mut bytes = [0xff; 128];
        unsafe { self.inner.raw.get_error_message(bytes.as_mut_ptr()) };
        parse_string("driver.error_message", &bytes)
    }

    pub fn channel_counts(&self) -> Result<dto::ChannelCounts> {
        self.require_query("get_channels")?;
        let (mut input, mut output) = (-1, -1);
        check_code("get_channels", unsafe {
            self.inner.raw.get_channels(&raw mut input, &raw mut output)
        })?;
        if input < 0 || output < 0 {
            return Err(invalid_response(
                "get_channels",
                "negative or unwritten channel count",
            ));
        }
        Ok(dto::ChannelCounts {
            in_: input,
            out: output,
        })
    }

    pub fn latencies(&self) -> Result<dto::Latencies> {
        self.require_query("get_latencies")?;
        let (mut input, mut output) = (-1, -1);
        check_code("get_latencies", unsafe {
            self.inner
                .raw
                .get_latencies(&raw mut input, &raw mut output)
        })?;
        if input < 0 || output < 0 {
            return Err(invalid_response(
                "get_latencies",
                "negative or unwritten latency",
            ));
        }
        Ok(dto::Latencies {
            in_: input,
            out: output,
        })
    }

    pub fn buffer_size(&self) -> Result<dto::BufferSize> {
        self.require_query("get_buffer_size")?;
        let (mut min, mut max, mut preferred, mut granularity) = (-1, -1, -1, -2);
        check_code("get_buffer_size", unsafe {
            self.inner.raw.get_buffer_size(
                &raw mut min,
                &raw mut max,
                &raw mut preferred,
                &raw mut granularity,
            )
        })?;
        if min <= 0 || max < min || !(min..=max).contains(&preferred) {
            return Err(invalid_response(
                "get_buffer_size",
                "invalid range or preferred size",
            ));
        }
        let granularity = NonZeroI32::new(granularity)
            .map(Granularity::try_from)
            .transpose()?;
        match granularity {
            Some(Granularity::Linear { step }) if (preferred - min) % step.get() != 0 => {
                return Err(invalid_response(
                    "get_buffer_size",
                    "preferred size is off the step grid",
                ));
            }
            Some(Granularity::Exponential) if !(preferred as u32).is_power_of_two() => {
                return Err(invalid_response(
                    "get_buffer_size",
                    "preferred size is not a power of two",
                ));
            }
            _ => {}
        }
        Ok(dto::BufferSize {
            min,
            max,
            preferred,
            granularity,
        })
    }

    pub fn can_sample_rate(&self, sample_rate: sys::SampleRate) -> Result<()> {
        self.require_query("can_sample_rate")?;
        validate_rate(sample_rate, false)?;
        check_code("can_sample_rate", unsafe {
            self.inner.raw.can_sample_rate(sample_rate)
        })
    }

    pub fn get_sample_rate(&self) -> Result<sys::SampleRate> {
        self.require_query("get_sample_rate")?;
        let mut rate = f64::NAN;
        check_code("get_sample_rate", unsafe {
            self.inner.raw.get_sample_rate(&raw mut rate)
        })?;
        if !rate.is_finite() || rate <= 0.0 {
            return Err(invalid_response(
                "get_sample_rate",
                "rate must be finite and positive",
            ));
        }
        Ok(rate)
    }

    /// Zero requests external sync; otherwise the rate must be finite and positive.
    pub fn set_sample_rate(&mut self, sample_rate: sys::SampleRate) -> Result<()> {
        self.require("set_sample_rate", self.phase == Phase::Initialized)?;
        validate_rate(sample_rate, true)?;
        check_code("set_sample_rate", unsafe {
            self.inner.raw.set_sample_rate(sample_rate)
        })
    }

    /// One bounded call, with no retry or allocation based on the driver's output.
    /// Zero capacity is invalid; a driver may report an empty list.
    pub fn clock_sources(&self, capacity: usize) -> Result<Vec<sys::ClockSource>> {
        self.require_query("get_clock_sources")?;
        let mut count = i32::try_from(capacity)
            .ok()
            .filter(|count| *count > 0)
            .ok_or(Error::InvalidArgument {
                field: "clock capacity",
            })?;
        let mut clocks = Vec::new();
        clocks
            .try_reserve_exact(capacity)
            .map_err(|_| Error::Allocation)?;
        // 每个元素在调用前均有有效 Rust 位模式；未写项保留非法哨兵供返回后拒绝。
        clocks.resize_with(capacity, || sys::ClockSource {
            index: -1,
            associated_channel: -2,
            associated_group: -2,
            is_current_source: sys::Bool(-1),
            name: [0xff; 32],
        });
        check_code("get_clock_sources", unsafe {
            self.inner
                .raw
                .get_clock_sources(clocks.as_mut_ptr(), &raw mut count)
        })?;
        if count < 0 || count as usize > capacity {
            return Err(Error::CapacityExceeded {
                operation: "get_clock_sources",
                capacity,
                reported: count,
            });
        }
        // 只能缩短已初始化存储，绝不信任返回数量扩展 Vec 长度。
        clocks.truncate(count as usize);
        for (position, clock) in clocks.iter().enumerate() {
            parse_bool("clock.is_current_source", clock.is_current_source)?;
            parse_string("clock.name", &clock.name)?;
            if clock.index < 0
                || clock.associated_channel < -1
                || clock.associated_group < -1
                || clocks[..position]
                    .iter()
                    .any(|earlier| earlier.index == clock.index)
            {
                return Err(invalid_response(
                    "get_clock_sources",
                    "invalid or duplicate clock identity",
                ));
            }
        }
        Ok(clocks)
    }

    pub fn channel_info(&self, channel: ChannelId) -> Result<dto::ChannelInfoResponse> {
        self.require_query("get_channel_info")?;
        if channel.index < 0 {
            return Err(Error::InvalidArgument {
                field: "channel index",
            });
        }
        let mut info = sys::ChannelInfo {
            channel: channel.index,
            is_input: channel.input.into(),
            is_active: sys::Bool(-1),
            channel_group: -1,
            sample_type: sys::SampleType(i32::MIN),
            name: [0xff; 32],
        };
        check_code("get_channel_info", unsafe {
            self.inner.raw.get_channel_info(&raw mut info)
        })?;
        if info.channel != channel.index
            || parse_bool("channel.is_input", info.is_input)? != channel.input
        {
            return Err(invalid_response(
                "get_channel_info",
                "requested channel identity changed",
            ));
        }
        info.try_into()
    }

    /// Register one fixed set of buffers and callbacks for this owner's lifetime.
    ///
    /// # Safety
    /// Before entry, the host must install dispatch and keep every callback,
    /// dispatch target and reachable context alive until process exit, including
    /// after errors or Owner drop. Callbacks may arrive synchronously during
    /// create/start/stop, on other threads, or long after a failed call.
    /// They must not unwind or access this Owner reentrantly/concurrently.
    /// The driver must respect array capacities, only write buffer-info during
    /// this call, and never modify the callback table. It may retain both pointers.
    /// Buffer access requires the host's own format, size, alignment and
    /// synchronization proof; successful registration validates metadata only.
    pub unsafe fn create_buffers(
        &mut self,
        channels: &[ChannelId],
        frames: i32,
        callbacks: sys::Callbacks,
    ) -> Result<()> {
        self.require("create_buffers", self.phase == Phase::Initialized)?;
        let count = i32::try_from(channels.len())
            .ok()
            .filter(|count| *count > 0)
            .ok_or(Error::InvalidArgument {
                field: "channel count",
            })?;
        if frames <= 0 {
            return Err(Error::InvalidArgument {
                field: "buffer frames",
            });
        }
        for (position, channel) in channels.iter().enumerate() {
            if channel.index < 0 || channels[..position].contains(channel) {
                return Err(Error::InvalidArgument {
                    field: "negative or duplicate channel",
                });
            }
        }
        let mut infos = Vec::new();
        infos
            .try_reserve_exact(channels.len())
            .map_err(|_| Error::Allocation)?;
        infos.extend(channels.iter().map(|channel| sys::BufferInfo {
            is_input: channel.input.into(),
            channel_num: channel.index,
            buffers: [ptr::null_mut(); 2],
        }));
        let infos = infos.into_boxed_slice();
        let callbacks = Box::new(callbacks);
        // 所有分配先完成；Box::into_raw 以后直到 driver entry 没有可失败步骤。
        // Box 的移动会影响此前派生的指针，故长期注册不能仅依赖地址不变。
        let registration = Registration {
            infos: unsafe { NonNull::new_unchecked(Box::into_raw(infos)) },
            callbacks: unsafe { NonNull::new_unchecked(Box::into_raw(callbacks)) },
        };
        let infos_ptr = registration.infos.as_ptr().cast::<sys::BufferInfo>();
        let callbacks_ptr = registration.callbacks.as_ptr();
        // 先保留 raw 分配，再进入第三方；create 失败不能撤销驱动已保存的地址。
        self.registration = Some(registration);
        self.phase = Phase::Failed;
        check_code("create_buffers", unsafe {
            self.inner
                .raw
                .create_buffers(infos_ptr, count, frames, callbacks_ptr)
        })?;
        let registration = self
            .registration
            .as_ref()
            .expect("installed before driver entry");
        // create 已返回；unsafe 入口合同禁止驱动此后再写入 metadata。
        let infos = unsafe { registration.infos.as_ref() };
        for (info, requested) in infos.iter().zip(channels) {
            let buffers = info.buffers;
            if info.channel_num != requested.index
                || parse_bool("buffer.is_input", info.is_input)? != requested.input
                || buffers.iter().any(|buffer| buffer.is_null())
            {
                return Err(invalid_response(
                    "create_buffers",
                    "invalid identity or null buffer pointer",
                ));
            }
        }
        self.buffers_valid = true;
        self.phase = Phase::Registered;
        Ok(())
    }

    /// Validated pointer metadata, available after a successful create call.
    /// Raw audio-buffer dereferences are still unsafe, including after stop.
    pub fn buffer_infos(&self) -> Option<&[sys::BufferInfo]> {
        if self.buffers_valid {
            // 返回长度来自原分配，且只有完成 create 验证后才能形成共享借用。
            self.registration
                .as_ref()
                .map(|registration| unsafe { registration.infos.as_ref() })
        } else {
            None
        }
    }

    pub fn start(&mut self) -> Result<()> {
        self.require("start", self.phase == Phase::Registered)?;
        self.phase = Phase::Failed;
        check_code("start", unsafe { self.inner.raw.start() })?;
        self.phase = Phase::Running;
        Ok(())
    }

    /// Explicitly call stop once after registration, including failed create/start.
    /// A successful return does not establish callback quiescence. This call may
    /// block inside the driver; Drop never calls it implicitly.
    pub fn stop(&mut self) -> Result<()> {
        self.require("stop", self.registration_started() && !self.stop_returned)?;
        self.phase = Phase::Failed;
        let code = unsafe { self.inner.raw.stop() };
        self.stop_returned = true;
        check_code("stop", code)?;
        self.phase = Phase::StopReturned;
        Ok(())
    }

    pub fn future<T: AsioFuture>(&mut self, param: &mut T::Param) -> Result<()> {
        if T::MUTATES_CONFIGURATION {
            self.require("future", self.phase == Phase::Initialized)?;
        } else {
            self.require_query("future")?;
        }
        check_future_code(unsafe {
            self.inner
                .raw
                .future(T::SELECTOR, ptr::from_mut(param).cast())
        })
    }

    /// Borrow the actual interface without adding a COM reference.
    ///
    /// # Safety
    /// Raw calls/clones must preserve the owner's phase, exclusive control,
    /// registration storage, COM lifetime and callback synchronization contracts.
    /// Do not register, release, dispose, reinitialize or mutate configuration
    /// behind the owner. A clone must not escape the permitted lifetime.
    pub unsafe fn as_raw(&self) -> &sys::IIASIORedecl {
        &self.inner.raw
    }

    fn require(&self, operation: &'static str, allowed: bool) -> Result<()> {
        if allowed {
            Ok(())
        } else {
            Err(Error::InvalidState {
                operation,
                phase: self.phase,
            })
        }
    }

    fn require_query(&self, operation: &'static str) -> Result<()> {
        self.require(
            operation,
            matches!(
                self.phase,
                Phase::Initialized | Phase::Registered | Phase::Running | Phase::StopReturned
            ),
        )
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if self.registration.is_none() {
            // 尚未暴露注册指针，按 Inner 字段顺序正常释放。
            unsafe { ManuallyDrop::drop(&mut self.inner) };
        }
        // 注册后仅丢弃无析构副作用的 raw 指针，永不重建 Box。
        // ManuallyDrop 保留 driver 与 COM；此分支不调用第三方入口。
    }
}

fn invalid_response(operation: &'static str, detail: &'static str) -> Error {
    Error::InvalidResponse { operation, detail }
}

fn validate_rate(rate: f64, allow_external_sync: bool) -> Result<()> {
    if rate.is_finite() && (rate > 0.0 || (allow_external_sync && rate == 0.0)) {
        Ok(())
    } else {
        Err(Error::InvalidArgument {
            field: "sample rate",
        })
    }
}
