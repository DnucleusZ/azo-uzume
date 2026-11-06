use azo_sys::{utils::I64Split, *};
use std::ffi::c_void;

#[test]
fn ordinary_and_future_success_are_not_interchangeable() {
    assert_eq!(ResultCode::OK.ok(7), Ok(7));
    assert_eq!(ResultCode::SUCCESS.ok(7), Err(ResultCode::SUCCESS));
    assert_eq!(ResultCode::SUCCESS.future_ok(7), Ok(7));
    assert_eq!(ResultCode::OK.future_ok(7), Err(ResultCode::OK));
    for raw in [
        i32::MIN,
        -1000,
        -999,
        -998,
        -997,
        -996,
        -995,
        -994,
        -1,
        1,
        i32::MAX,
    ] {
        let code = ResultCode(raw);
        assert_eq!(code.ok(()), Err(code));
        assert_eq!(code.future_ok(()), Err(code));
    }
}

#[test]
fn unknown_integer_values_survive_the_raw_boundary() {
    for raw in [i32::MIN, -1, 41, 0x1234_5678, i32::MAX] {
        // 模拟原生端写出的完整 32 位值；任何位模式都必须是合法的 raw wrapper。
        let sample: SampleType = unsafe { std::mem::transmute(raw) };
        let result: ResultCode = unsafe { std::mem::transmute(raw) };
        let future: FutureSelector = unsafe { std::mem::transmute(raw) };
        let message: MessageSelector = unsafe { std::mem::transmute(raw) };
        let transport: TransportParametersCommand = unsafe { std::mem::transmute(raw) };
        let format: IoFormatType = unsafe { std::mem::transmute(raw) };
        assert_eq!(i32::from(sample), raw);
        assert_eq!(i32::from(result), raw);
        assert_eq!(i32::from(future), raw);
        assert_eq!(i32::from(message), raw);
        assert_eq!(i32::from(transport), raw);
        assert_eq!(i32::from(format), raw);
    }
    let unknown = SampleType(0x1234_5678);
    assert_eq!(format!("{unknown:?}"), "SampleType::<unknown>(305419896)");
}

#[test]
fn invalid_boolean_is_rejected_without_losing_raw_value() {
    assert_eq!(bool::try_from(Bool::FALSE), Ok(false));
    assert_eq!(bool::try_from(Bool::TRUE), Ok(true));
    for raw in [i32::MIN, -1, 2, 42, i32::MAX] {
        let value: Bool = unsafe { std::mem::transmute(raw) };
        assert_eq!(value.0, raw);
        assert_eq!(bool::try_from(value), Err(UndefinedValueError));
    }
}

#[test]
fn unknown_time_flags_are_retained() {
    let bits = 0x8000_00a1;
    assert_eq!(TimeInfoFlags::from_bits_retain(bits).bits(), bits);
    assert_eq!(TimeCodeFlags::from_bits_retain(bits).bits(), bits);
    let info: TimeInfoFlags = unsafe { std::mem::transmute(bits) };
    let code: TimeCodeFlags = unsafe { std::mem::transmute(bits) };
    assert_eq!(info.bits(), bits);
    assert_eq!(code.bits(), bits);
}

#[test]
fn split_words_preserve_signed_and_unsigned_bits_and_sdk_order() {
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert_eq!(i64::from(I64Split::from(value)), value);
    }
    for value in [0, 1, 0x0123_4567_89ab_cdef, 1 << 63, u64::MAX] {
        assert_eq!(u64::from(I64Split::from(value)), value);
    }
    let split = I64Split::from(0x0123_4567_89ab_cdef_u64);
    assert_eq!((split.hi, split.lo), (0x0123_4567, 0x89ab_cdef));
    let bytes: [u8; 8] = unsafe { std::mem::transmute(split) };
    assert_eq!(bytes, [0x67, 0x45, 0x23, 0x01, 0xef, 0xcd, 0xab, 0x89]);
}

#[test]
fn callbacks_use_c_signatures_and_writable_message_arguments() {
    unsafe extern "C" fn message(
        selector: MessageSelector,
        value: i32,
        data: *mut c_void,
        opt: *mut f64,
    ) -> i32 {
        unsafe {
            *data.cast::<i32>() = selector.0;
            *opt = 44_100.0;
        }
        value
    }
    let callbacks = Callbacks {
        asio_message: message,
        ..Callbacks::NOOP
    };
    let mut data = 0;
    let mut opt = 0.0;
    let message_fn = callbacks.asio_message;
    let buffer_switch: BufferSwitch = callbacks.buffer_switch;
    let rate_change: SampleRateDidChange = callbacks.sample_rate_did_change;
    let time_switch = callbacks.buffer_switch_time_info.unwrap();
    let mut time = Time::default();
    unsafe {
        buffer_switch(1, Bool::FALSE);
        rate_change(0.0);
        assert_eq!(
            message_fn(
                MessageSelector(i32::MIN),
                -7,
                (&mut data as *mut i32).cast(),
                &mut opt
            ),
            -7
        );
        assert_eq!(
            time_switch(&mut time, 0, Bool::TRUE),
            &mut time as *mut Time
        );
    }
    assert_eq!(data, i32::MIN);
    assert_eq!(opt, 44_100.0);
}
