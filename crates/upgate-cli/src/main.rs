fn main() {
    match upgate_cli::run_from_env() {
        Ok(output) => print!("{output}"),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(if err.is_interruption() { 130 } else { 1 });
        }
    }
}
