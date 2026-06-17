const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner_frame(spinner_tick: usize) -> &'static str {
    SPINNER[spinner_tick % SPINNER.len()]
}
