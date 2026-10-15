//! Prepare and print a launch plan without launching Wine or the game.
use skinos_compat::{GameSettings, PrepareRequest, prepare_launch};
use std::path::PathBuf;
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(
        args.len() == 5,
        "usage: prepare APPID INSTALL_DIR EXECUTABLE CROSSOVER_APP"
    );
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let result = prepare_launch(&PrepareRequest {
        data_dir: home.join("Library/Application Support/SkinOs"),
        steamapps_dir: home.join("Library/Application Support/Steam/steamapps"),
        home,
        appid: args[1].parse().unwrap(),
        install_dir: (&args[2]).into(),
        executable: (&args[3]).into(),
        crossover_path: Some((&args[4]).into()),
        arguments: Vec::new(),
        working_dir: PathBuf::from("."),
        settings: GameSettings::default(),
    });
    match result {
        Ok(plan) => println!("{}", serde_json::to_string_pretty(&plan).unwrap()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
