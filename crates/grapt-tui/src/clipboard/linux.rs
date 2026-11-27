//! ICCCM selection transfer: request only the remaining bounded property bytes.
//! This runs in the acquisition child; its parent also bounds completion time.
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{AtomEnum, ConnectionExt, CreateWindowAux, EventMask, Property, WindowClass},
        Event,
    },
    COPY_DEPTH_FROM_PARENT, CURRENT_TIME,
};

const UNAVAILABLE: &str = "Clipboard has no readable text";
const TOO_LARGE: &str = "Clipboard exceeds 64 KiB; paste refused";

pub(super) fn read_text() -> Result<String, &'static str> {
    let (connection, screen) = x11rb::connect(None).map_err(|_| UNAVAILABLE)?;
    let atom = |name: &[u8]| -> Result<u32, &'static str> {
        connection
            .intern_atom(false, name)
            .map_err(|_| UNAVAILABLE)?
            .reply()
            .map(|reply| reply.atom)
            .map_err(|_| UNAVAILABLE)
    };
    let clipboard = atom(b"CLIPBOARD")?;
    let utf8 = atom(b"UTF8_STRING")?;
    let incr = atom(b"INCR")?;
    let property = atom(b"GRAPT_CLIPBOARD_TEXT")?;
    let window = connection.generate_id().map_err(|_| UNAVAILABLE)?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or(UNAVAILABLE)?
        .root;
    connection
        .create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|_| UNAVAILABLE)?;
    connection
        .convert_selection(window, clipboard, utf8, property, CURRENT_TIME)
        .map_err(|_| UNAVAILABLE)?;
    connection.flush().map_err(|_| UNAVAILABLE)?;
    let mut incremental = false;
    let mut bytes = Vec::with_capacity(crate::MAX_DRAFT);
    for _ in 0..4096 {
        let selected = match connection.wait_for_event().map_err(|_| UNAVAILABLE)? {
            Event::SelectionNotify(event) if event.requestor == window => {
                if event.property != property {
                    return Err(UNAVAILABLE);
                }
                true
            }
            Event::PropertyNotify(event)
                if incremental
                    && event.window == window
                    && event.atom == property
                    && event.state == Property::NEW_VALUE =>
            {
                false
            }
            _ => continue,
        };
        // X11's long_length is in 4-byte units. At most three padding bytes
        // beyond the remaining budget can reach the reply allocation.
        let remaining = crate::MAX_DRAFT.saturating_sub(bytes.len());
        let length = remaining.div_ceil(4).max(1) as u32;
        let reply = connection
            .get_property(!selected, window, property, AtomEnum::ANY, 0, length)
            .map_err(|_| UNAVAILABLE)?
            .reply()
            .map_err(|_| UNAVAILABLE)?;
        if reply.bytes_after != 0 || reply.value.len() > remaining {
            return Err(TOO_LARGE);
        }
        if selected && reply.type_ == incr {
            if reply
                .value32()
                .and_then(|mut values| values.next())
                .ok_or(UNAVAILABLE)? as usize
                > crate::MAX_DRAFT
            {
                return Err(TOO_LARGE);
            }
            incremental = true;
            // Deleting the INCR property acknowledges that the owner may send.
            connection
                .delete_property(window, property)
                .map_err(|_| UNAVAILABLE)?;
            connection.flush().map_err(|_| UNAVAILABLE)?;
            continue;
        }
        if reply.format != 8 || reply.type_ != utf8 {
            return Err(UNAVAILABLE);
        }
        let complete = !incremental || reply.value.is_empty();
        bytes.extend_from_slice(&reply.value);
        if complete {
            return String::from_utf8(bytes).map_err(|_| UNAVAILABLE);
        }
        connection.flush().map_err(|_| UNAVAILABLE)?;
    }
    Err(UNAVAILABLE)
}
