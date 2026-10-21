//! `bml`, porting `bps/cli.py`'s `main`.

use bytemill::{cli, cli_help};
use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match cli::main(&argv) {
        Ok(code) => ExitCode::from(code as u8),
        Err(usage) => {
            eprintln!("{usage}");
            if let Some(hint) = cli_help::hint(&usage) {
                eprintln!("{hint}");
            }
            ExitCode::from(2)
        }
    }
}
