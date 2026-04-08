mod brew;

fn main() {
    if let Err(err) = brew::run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
