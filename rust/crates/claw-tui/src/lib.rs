mod app;
mod config;
mod markdown;
mod slash;
mod terminal;
mod theme;

use std::io;

/// Run the full-screen Claw Code frontend.
pub fn run() -> io::Result<()> {
    app::run()
}
