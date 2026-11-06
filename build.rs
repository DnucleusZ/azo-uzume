use std::env;

fn main() {
    let out_dir = env::var("OUT_DIR").expect("env var `OUT_DIR` should be set by cargo");

    let mut builder = windows_bindgen::builder();

    builder.filters([
        "CoInitializeEx",
        "COINIT_APARTMENTTHREADED",
        "CoCreateInstance",
        "CLSCTX_INPROC_SERVER",
        "S_OK",
        "S_FALSE",
        "E_POINTER",
        "HWND",
        "CoUninitialize",
    ]);

    builder
        .flat()
        .dead_code()
        .output(format!("{out_dir}/windows_bindgen_output.rs"))
        .write();
}
