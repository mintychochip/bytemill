//! `scrape_wiki_routes`, porting `scripts/scrape_wiki_routes.py`.

use bps::wiki_routes;
use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match wiki_routes::main(&argv) {
        Ok(code) => ExitCode::from(code as u8),
        Err(usage) => {
            eprintln!("{usage}");
            ExitCode::from(2)
        }
    }
}
