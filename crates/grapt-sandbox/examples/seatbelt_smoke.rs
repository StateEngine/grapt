//! Native adapter acceptance, invoked explicitly by the private macOS workflow.
#[cfg(target_os = "macos")]
#[path = "seatbelt_smoke/driver.rs"]
mod driver;

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "macos")]
    {
        match driver::main() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                std::process::ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("Seatbelt native acceptance requires macOS");
        std::process::ExitCode::from(77)
    }
}
