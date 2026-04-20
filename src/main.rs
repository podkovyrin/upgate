pub(crate) mod app;
pub(crate) mod config;
pub(crate) mod interactive;
pub(crate) mod managers;
pub(crate) mod outcome;
pub(crate) mod ui;
pub(crate) mod util;

fn main() {
    let exit_code = app::run();
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
