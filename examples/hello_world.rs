use azo::driver::Metadata;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for driver in Metadata::enumerate()? {
        println!("{:?}: {:?}", driver.clsid, driver.description);
    }
    Ok(())
}
