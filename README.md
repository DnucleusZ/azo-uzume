# azo-uzume

ASIO bindings for Windows x64 MSVC, derived from [LastExceed/azo](https://github.com/LastExceed/azo) and independently maintained by DnucleusZ. The upstream MIT notices and public history are retained. [UPSTREAM.md](UPSTREAM.md) records the fixed source baseline and the downstream scope.

The `azo` and `azo-sys` packages use the downstream version `0.6.0-dev.1`. Both are distributed from Git and have `publish = false`. Consumers must pin a reviewed full commit SHA; this version has its own API and does not imply compatibility with an upstream release.

### Scope

`azo-sys` provides the raw ASIO COM interface, four-byte-packed structures, callback types and integer wrappers that preserve unknown values. Normal builds use Rust and Windows metadata; they require the MSVC/Windows SDK toolchain but do not include or build the Steinberg ASIO SDK.

`azo::driver::Owner` controls one driver on its COM owner thread. It is neither cloneable nor transferable between threads. It validates query results, distinguishes ordinary ASIO success from future success, and owns stable callback and buffer-registration storage. The three typed IO-format operations are `GetIoFormat`, `CanDoIoFormat` and `SetIoFormat`.

The application owns audio dispatch, sample interpretation, queueing, message pumping on the owner thread and process supervision. This library does not add an audio queue or a control-proxy thread. Driver calls are synchronous and may block indefinitely; a process boundary is required when the application needs a hard termination limit.

### Registration lifetime

Before entering `create_buffers`, the application must publish its callback dispatch and make all callback-accessible resources valid for the documented lifetime. The library holds its registration arrays and callback table at stable addresses before calling the driver. A driver may call back synchronously during create, start or stop, including calls that ultimately fail.

Once registration has been attempted, the owner preserves the registration, driver reference and COM lifetime through process termination. Returning from stop does not prove that notifications or delegated work have become quiescent. Dropping a registered owner therefore preserves those resources and does not call blocking driver cleanup methods. An owner that never attempted registration releases its driver before uninitializing COM.

The application may separately close its own audio-access gate and reclaim audio objects once it has established their access boundary. That action does not release the driver's callback registration. Raw interface access and injection are explicit unsafe operations with their own lifetime requirements.

### Build and software checks

The tested toolchain is pinned by [rust-toolchain.toml](rust-toolchain.toml). From a fresh checkout, first fetch the locked dependencies online, then run offline checks:

```bash
cargo fetch --locked
cargo test --workspace --locked --offline
cargo test --workspace --features undocumented --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo clippy --workspace --all-targets --features undocumented --locked --offline -- -D warnings
cargo fmt --all --check
```

The optional `undocumented` feature exposes additional raw ABI definitions. The safe IO-format API remains restricted to its three typed operations. Layout, raw-return and mock-driver tests are software checks and do not establish physical playback or sustained performance.

The external SDK ABI test is explicitly ignored by ordinary test commands. To run it, independently build the matching native oracle, set `AZO_SDK_ABI_DLL` to its absolute path, and execute:

```bash
cargo test -p azo-sys --test sdk_abi --locked --offline -- --ignored --nocapture
cargo test -p azo-sys --test sdk_abi --features undocumented --locked --offline -- --ignored --nocapture
```

The SDK and oracle DLL are supplied separately. A missing DLL is an error when this test is explicitly requested. Its result covers the selected SDK, DLL, target and library build.

### Examples

`cargo run --example hello_world --locked --offline` lists registered driver identities without loading a driver. `cargo run --example inspect --locked --offline -- <CLSID>` loads and initializes the explicitly selected driver, prints individual query results, and releases the unregistered owner. It does not create buffers or start audio. Run third-party driver inspection under appropriate process supervision.
