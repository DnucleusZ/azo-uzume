use azo::driver::Owner;
use azo::windows_core::GUID;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argument = std::env::args().nth(1).ok_or("usage: inspect <CLSID>")?;
    let clsid: GUID = argument.trim_matches(['{', '}']).try_into()?;
    let mut owner = Owner::new(&clsid)?;
    owner.init(None)?;
    // 独立诊断失败保留原错误；本示例不创建缓冲或启动流。
    println!("name: {:?}", owner.name());
    println!("version: {:?}", owner.version());
    println!("channels: {:?}", owner.channel_counts());
    println!("latencies: {:?}", owner.latencies());
    println!("buffer: {:?}", owner.buffer_size());
    println!("rate: {:?}", owner.get_sample_rate());
    println!("clocks: {:?}", owner.clock_sources(64));
    Ok(())
}
