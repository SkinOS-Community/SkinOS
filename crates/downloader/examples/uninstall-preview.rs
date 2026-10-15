//! Inspect removal scope without executing deletion or connecting to Steam.
use anyhow::{Context, Result};
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let steamapps = PathBuf::from(args.next().context("Expected steamapps path")?);
    let legacy = PathBuf::from(args.next().context("Expected legacy games path")?);
    let mut plans = Vec::new();
    for app in args {
        plans.push(skinos_downloader::library::uninstall_plan(
            &steamapps,
            app.parse()?,
            Some(&legacy),
        )?);
    }
    println!("{}", serde_json::to_string_pretty(&plans)?);
    Ok(())
}
