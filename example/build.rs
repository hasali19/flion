use std::error::Error;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    flion_build::generate_plugins_registrant(Path::new("."))
}
