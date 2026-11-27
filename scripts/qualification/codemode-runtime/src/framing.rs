//! Optional qualification handshake: prove all guest runtimes exist before release.
use std::io::{Read, Write};

pub fn source(maximum: usize) -> Result<(bool, Vec<u8>), Box<dyn std::error::Error>> {
    let framed = matches!(
        std::env::args().nth(1).as_deref(),
        Some("--framed" | "--batch" | "--pending")
    );
    let mut input = Vec::new();
    if framed {
        let mut length = [0; 4];
        std::io::stdin().read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > maximum {
            return Err("framed source exceeds input bound".into());
        }
        input.resize(length, 0);
        std::io::stdin().read_exact(&mut input)?;
    } else {
        std::io::stdin()
            .take((maximum + 1) as u64)
            .read_to_end(&mut input)?;
    }
    Ok((framed, input))
}

pub fn ready(framed: bool) -> Result<(), Box<dyn std::error::Error>> {
    if framed {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(b"READY\n")?;
        stdout.flush()?;
        let mut release = [0];
        std::io::stdin().read_exact(&mut release)?;
        if release != [1] {
            return Err("invalid qualification barrier release".into());
        }
    }
    Ok(())
}
