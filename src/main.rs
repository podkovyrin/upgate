mod app;
mod config;
mod interactive;
mod manager;
mod managers;
mod outcome;
mod ui;
mod util;

fn main() {
    let exit_code = app::run();
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
