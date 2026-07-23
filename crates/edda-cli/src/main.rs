//! The `edda` binary. See the `edda_cli` library crate for the
//! command-line surface; this file is a thin entry point that wires
//! `std::env::args()` and `std::io::stderr` to [`edda_cli::run`].

use std::process::ExitCode;

use edda_cli::{DriverDispatcher, run};
use edda_diag::Diagnostics;
use edda_driver::render_diagnostic;

const MAIN_STACK_SIZE_BYTES: usize = 256 * 1024 * 1024;

fn main() -> ExitCode {
    std::thread::Builder::new()
        .stack_size(MAIN_STACK_SIZE_BYTES)
        .spawn(run_and_render)
        .expect("failed to spawn main-work thread")
        .join()
        .expect("main-work thread panicked")
}

/// Parse argv, dispatch the verb, and render the resulting diagnostics
/// take. Split out of `main` so it can run on a thread with a larger
/// stack than the OS default main-thread stack.
fn run_and_render() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let mut diags = Diagnostics::new();
    let mut dispatcher = DriverDispatcher::new();
    let code = run(&argv, &mut dispatcher, &mut diags);
    render_diagnostics(&diags);
    code
}

/// Render every diagnostic in the take using the §6 multi-line surface
/// format. Each rendered block already ends with a newline (writeln! in
/// the renderer), so consecutive diagnostics print with one blank line
/// of separation via the trailing `eprintln!()`.
fn render_diagnostics(diags: &Diagnostics) {
    for d in diags.iter() {
        eprint!("{}", render_diagnostic(d));
        eprintln!();
    }
}
