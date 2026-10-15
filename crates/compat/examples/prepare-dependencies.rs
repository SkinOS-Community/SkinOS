//! Explicit, account-free bootstrap diagnostic for a chosen data directory.
fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(directory) = args.next() else {
        eprintln!("Usage: prepare-dependencies DATA_DIRECTORY");
        std::process::exit(2);
    };
    if args.next().is_some() {
        eprintln!("Expected exactly one data directory");
        std::process::exit(2);
    }
    match skinos_compat::bootstrap::prepare_dependencies(std::path::Path::new(&directory)) {
        Ok(path) => println!("{}", path.display()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
