//! Development preview: cargo run -p grapt-tui --example gateway
//! No engine calls or binding authority. The normal native binary is untouched.
#[path = "gateway/fixture.rs"]
mod fixture;
#[path = "../src/gateway/mod.rs"]
pub mod gateway;

use crossterm::{
    event::{Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use gateway::{Availability, Gateway};
use ratatui::{backend::CrosstermBackend, Terminal};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let availability = if std::env::args().any(|arg| arg == "--disconnected") {
        Availability::Disconnected
    } else {
        Availability::Connected
    };
    let Some(mut gateway) = Gateway::open(availability) else {
        println!("StateEngine unavailable; continue with the ordinary session.");
        return Ok(());
    };
    enable_raw_mode()?;
    let result = run(&mut gateway);
    let raw = disable_raw_mode();
    let screen = execute!(std::io::stdout(), LeaveAlternateScreen);
    result?;
    raw?;
    screen?;
    Ok(())
}
fn run(gateway: &mut Gateway) -> Result<(), Box<dyn std::error::Error>> {
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    loop {
        if let Some((ticket, request)) = gateway.pending() {
            let reply = fixture::read(request)?;
            gateway.accept(ticket, Ok(reply));
        }
        terminal.draw(|frame| gateway.draw(frame, frame.area()))?;
        if let Event::Key(key) = crossterm::event::read()? {
            if (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
                || gateway.key(key.code)
            {
                break;
            }
        }
    }
    Ok(())
}
