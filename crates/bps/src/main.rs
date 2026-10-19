//! `bml`, porting `bps/cli.py`'s `main`.

use bps::cli;
use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match cli::main(&argv) {
        Ok(code) => ExitCode::from(code as u8),
        Err(usage) => {
            eprintln!("{usage}");
            ExitCode::from(2)
        }
    }
}
