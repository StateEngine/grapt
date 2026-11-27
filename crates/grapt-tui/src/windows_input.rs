//! Preserve the VT parser, reading safe Win32 records for resize and focus.
//! One console reader; no polling timer while idle and no competing stdin read.
use crate::input_bytes::Parser;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm_winapi::{Console, ConsoleMode, Handle, HandleType, InputRecord};
use std::sync::mpsc::{self, Receiver, SyncSender};

/// Keep console mode changes on the UI thread so shutdown cannot race a
/// reader that enables VT input after the terminal has already been restored.
pub struct Mode<'a> {
    access: &'a grapt_console::Access,
    console: ConsoleMode,
    original: u32,
}

impl<'a> Mode<'a> {
    pub fn enter(access: &'a grapt_console::Access) -> Result<Self, String> {
        let handle = Handle::new(HandleType::InputHandle).map_err(|e| e.to_string())?;
        let console = ConsoleMode::from(handle);
        let original = console.mode().map_err(|e| e.to_string())?;
        console
            .set_mode((original | 0x0200 | 0x0008 | 0x0010 | 0x0080) & !0x0040)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            access,
            console,
            original,
        })
    }
}

impl Drop for Mode<'_> {
    fn drop(&mut self) {
        self.access.with_output(|| {
            let _ = self.console.set_mode(self.original);
        });
    }
}

type NativeRecord = std::io::Result<InputRecord>;

// Drain only records already available; a single key never waits for another.
// Keep a bounded read atomic through decoding so one burst needs one echo paint.
fn read_records(
    records: &SyncSender<Vec<NativeRecord>>,
    wake: &SyncSender<()>,
    activity: &crate::startup_input::State,
) -> std::io::Result<()> {
    let handle = Handle::new(HandleType::InputHandle)?;
    let console = Console::from(handle);
    let limit = crate::startup_input::batch::LIMIT;
    loop {
        let mut batch = Vec::with_capacity(limit);
        let mut stopped = false;
        for _ in 0..limit {
            let record = console.read_single_input_event();
            stopped = record.is_err();
            if matches!(&record, Ok(InputRecord::KeyEvent(key)) if key.key_down)
                || matches!(&record, Ok(InputRecord::MouseEvent(mouse)) if mouse.event_flags == crossterm_winapi::EventFlags::MouseWheeled || mouse.button_state.left_button())
            {
                activity.activity(wake);
            } else if activity.observing() {
                let _ = wake.try_send(());
            }
            batch.push(record);
            if stopped {
                break;
            }
            match console.number_of_console_input_events() {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) => {
                    if records.send(batch).is_err() {
                        return Ok(());
                    }
                    return Err(error);
                }
            }
        }
        if records.send(batch).is_err() {
            return Ok(());
        }
        if stopped {
            return Ok(());
        }
    }
}

#[cfg_attr(windows, expect(clippy::cognitive_complexity, reason = "legacy"))]
#[cfg_attr(windows, expect(clippy::too_many_lines, reason = "legacy"))]
pub fn start(
    wake: SyncSender<()>,
    activity: std::sync::Arc<crate::startup_input::State>,
) -> Result<Receiver<crate::startup_input::batch::Packet>, String> {
    let (output, receiver) = mpsc::sync_channel(1);
    let (records, input) = mpsc::sync_channel(1);
    let reader_wake = wake.clone();
    let reader_activity = activity.clone();
    let _reader = std::thread::Builder::new()
        .name("tui-r-console-records".into())
        .spawn(move || {
            let result = read_records(&records, &reader_wake, &reader_activity);
            if let Err(error) = result {
                let error = Err(error);
                let _ = records.send(vec![error]);
            }
        })
        .map_err(|e| e.to_string())?;
    let _decoder = std::thread::Builder::new()
        .name("tui-r-console-decode".into())
        .spawn(move || {
            let mut parser = Parser::new();
            let started = tokio::time::Instant::now();
            let mut high = None;
            loop {
                let generation = activity.generation();
                let polled = if activity.observing() {
                    crate::record_input::poll_for(
                        &mut parser,
                        &input,
                        || started.elapsed(),
                        std::time::Duration::from_millis(5),
                    )
                } else {
                    crate::record_input::poll(&mut parser, &input, || started.elapsed())
                };
                let (record, events) = match polled {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = output.send(vec![Err(error)]);
                        let _ = wake.try_send(());
                        return;
                    }
                };
                let mut expired = crate::startup_input::batch::Batch::new(&output, &wake);
                for event in events {
                    if !expired.push(event) {
                        return;
                    }
                }
                if !expired.flush() {
                    return;
                }
                let Some(records) = record else {
                    if activity.observing() && activity.requested() {
                        let empty = match Handle::new(HandleType::InputHandle).and_then(|handle| {
                            Console::from(handle).number_of_console_input_events()
                        }) {
                            Ok(count) => count == 0,
                            Err(error) => {
                                let _ = output.send(vec![Err(error.to_string())]);
                                let _ = wake.try_send(());
                                return;
                            }
                        };
                        activity.acknowledge(
                            parser.idle() && high.is_none() && empty,
                            generation,
                            &wake,
                        );
                    }
                    continue;
                };
                let mut batch = crate::startup_input::batch::Batch::new(&output, &wake);
                for record in records {
                    let mut events = Vec::new();
                    match record {
                        Err(error) => {
                            let _ = batch.push(Err(error.to_string()));
                            let _ = batch.flush();
                            return;
                        }
                        Ok(InputRecord::MouseEvent(mouse)) => {
                            let mut modifiers = KeyModifiers::NONE;
                            if mouse.control_key_state.has_state(0x0010) {
                                modifiers |= KeyModifiers::SHIFT;
                            }
                            if mouse.control_key_state.has_state(0x0003) {
                                modifiers |= KeyModifiers::ALT;
                            }
                            if mouse.control_key_state.has_state(0x000c) {
                                modifiers |= KeyModifiers::CONTROL;
                            }
                            if let Some(event) = crate::mouse::native(
                                mouse.event_flags,
                                mouse.button_state,
                                mouse.mouse_position,
                                modifiers,
                            ) {
                                events.push(event);
                            }
                        }
                        Ok(InputRecord::FocusEvent(record)) => {
                            events.push(crate::record_input::focus(record.set_focus));
                        }
                        Ok(InputRecord::WindowBufferSizeEvent(_)) => {
                            if let Ok((width, height)) = crossterm::terminal::size() {
                                events.push(Event::Resize(width, height));
                            }
                        }
                        Ok(InputRecord::KeyEvent(key)) if key.key_down => {
                            let mut modifiers = KeyModifiers::NONE;
                            if key.control_key_state.has_state(0x0010) {
                                modifiers |= KeyModifiers::SHIFT;
                            }
                            if key.control_key_state.has_state(0x000c) {
                                modifiers |= KeyModifiers::CONTROL;
                            }
                            if key.control_key_state.has_state(0x0003) {
                                modifiers |= KeyModifiers::ALT;
                            }
                            let special = match key.virtual_key_code {
                                13 if modifiers.contains(KeyModifiers::SHIFT) => {
                                    Some(KeyCode::Enter)
                                }
                                33 => Some(KeyCode::PageUp),
                                34 => Some(KeyCode::PageDown),
                                35 => Some(KeyCode::End),
                                36 => Some(KeyCode::Home),
                                37 => Some(KeyCode::Left),
                                38 => Some(KeyCode::Up),
                                39 => Some(KeyCode::Right),
                                40 => Some(KeyCode::Down),
                                46 => Some(KeyCode::Delete),
                                code => crate::record_input::function_key(code),
                            };
                            for _ in 0..key.repeat_count.max(1) {
                                if let Some(code) = special {
                                    events.push(Event::Key(KeyEvent::new(code, modifiers)));
                                    continue;
                                }
                                let unit = key.u_char;
                                if unit == 0 {
                                    continue;
                                }
                                if (0xd800..=0xdbff).contains(&unit) {
                                    high = Some(unit);
                                    continue;
                                }
                                let ch = if let Some(first) = high.take() {
                                    char::decode_utf16([first, unit])
                                        .next()
                                        .and_then(Result::ok)
                                } else {
                                    char::from_u32(u32::from(unit))
                                };
                                let Some(ch) = ch else {
                                    if !batch.push(Err(
                                        "Invalid UTF-16 console input; draft kept".into()
                                    )) {
                                        return;
                                    }
                                    continue;
                                };
                                let mut buffer = [0; 4];
                                let vt = parser.feed(
                                    ch.encode_utf8(&mut buffer).as_bytes(),
                                    started.elapsed(),
                                );
                                for event in vt {
                                    if !batch.push(event) {
                                        return;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    for event in events {
                        if !batch.push(Ok(event)) {
                            return;
                        }
                    }
                }
                if !batch.flush() {
                    return;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows input console; native workers redirect stdin"]
    fn native_mode_restores_original_flags() -> Result<(), String> {
        let console =
            ConsoleMode::from(Handle::new(HandleType::InputHandle).map_err(|e| e.to_string())?);
        let original = console.mode().map_err(|e| e.to_string())?;
        {
            let access = grapt_console::Access::default();
            let _mode = Mode::enter(&access)?;
            crossterm::terminal::enable_raw_mode().map_err(|e| e.to_string())?;
            let active = console.mode().map_err(|e| e.to_string())?;
            assert_eq!(active & 0x0298, 0x0298, "VT, mouse and resize input flags");
            assert_eq!(active & 0x0047, 0, "raw input with quick edit disabled");
            crossterm::terminal::disable_raw_mode().map_err(|e| e.to_string())?;
        }
        assert_eq!(console.mode().map_err(|e| e.to_string())?, original);
        Ok(())
    }
}
