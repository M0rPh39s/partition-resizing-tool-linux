//! partresize — safety-first Linux partition & filesystem resizing.

mod cli;
mod crypt;
mod discovery;
mod errors;
mod executor;
mod filesystem;
mod lvm;
mod partition;
mod planner;
mod recovery;
mod safety;
mod schedule;
mod storage;
mod util;
mod verification;

fn main() -> std::process::ExitCode {
    match cli::run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::from(1)
        }
    }
}
