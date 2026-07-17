mod app;
mod config;
mod markdown;
mod slash;
mod terminal;
mod theme;

use std::io;

fn main() -> io::Result<()> {
    if std::env::args().any(|argument| matches!(argument.as_str(), "-h" | "--help")) {
        println!("claw-tui — standalone Claw Code full-screen UI demo");
        println!();
        println!("Runs a mock streaming conversation in an alternate screen.");
        println!(
            "Keys: Enter send · Shift+Tab cycle mode · Ctrl+C/Esc quit · PageUp/PageDown scroll"
        );
        return Ok(());
    }

    app::run()
}
