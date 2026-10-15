use skinos_runtime_probe::{discover_runtime, inspect};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "Usage: skinos-runtime-probe inspect [--runtime PATH] [--json]\n\nStatically inspects a local macOS steamclient.dylib without requiring Xcode command-line tools.\nDoes not load Valve code, start Steam, log in, or read account files.";

fn execute() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        return Err(USAGE.into());
    };
    if command == "--help" || command == "-h" {
        println!("{USAGE}");
        return Ok(());
    }
    if command != "inspect" {
        return Err(format!(
            "Unknown command: {}\n{USAGE}",
            command.to_string_lossy()
        ));
    }
    let mut runtime: Option<PathBuf> = None;
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--runtime") if runtime.is_none() => {
                let path: OsString = args.next().ok_or("--runtime requires a library path.")?;
                runtime = Some(path.into());
            }
            Some("--json") if !json => json = true,
            Some("--help" | "-h") => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => {
                return Err(format!(
                    "Unknown or repeated option: {}\n{USAGE}",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    let runtime = runtime.map(Ok).unwrap_or_else(discover_runtime)?;
    let report = inspect(&runtime)?;
    let mut stdout = io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut stdout, &report)
            .map_err(|error| format!("Could not write JSON report: {error}"))?;
        writeln!(stdout).map_err(|error| error.to_string())?;
    } else {
        writeln!(stdout, "Runtime: {}", report.canonical_path.display())
            .map_err(|error| error.to_string())?;
        writeln!(stdout, "SHA-256: {}", report.sha256).map_err(|error| error.to_string())?;
        for architecture in &report.architectures {
            let found = architecture
                .target_exports
                .iter()
                .filter(|export| export.present)
                .count();
            writeln!(
                stdout,
                "{}: {found}/{} target exports; {} declared dependencies; initializer sections: {}",
                architecture.name,
                architecture.target_exports.len(),
                architecture.dependencies.len(),
                if architecture.initializer_section_present {
                    architecture.initializer_sections.join(", ")
                } else {
                    "none found".into()
                }
            )
            .map_err(|error| error.to_string())?;
        }
        writeln!(
            stdout,
            "Compatibility: unverified. Static inspection only; runtime was not initialized."
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
