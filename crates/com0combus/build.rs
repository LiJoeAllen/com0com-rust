fn main() -> Result<(), Box<dyn std::error::Error>> {
    wdk_build::configure_wdk_binary_build()?;
    Ok(())
}
