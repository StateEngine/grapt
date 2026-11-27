//! Bounded process output with a stable prefix, recent suffix and absolute cursors.
//!
//! Modified by Grapt: nondestructive cursor pages and explicit omitted byte counts.
use std::collections::VecDeque;

const CAPACITY: usize = 512 * 1024;
const HEAD: usize = CAPACITY / 2;
const TAIL: usize = CAPACITY - HEAD;

#[derive(Debug, Default)]
pub struct OutputBuffer {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
}

#[derive(Debug)]
pub struct Page {
    pub bytes: Vec<u8>,
    pub omitted: u64,
    pub omitted_at: Option<usize>,
    pub cursor: u64,
}

impl OutputBuffer {
    pub fn append(&mut self, chunk: &[u8]) -> Result<(), &'static str> {
        self.total = self
            .total
            .checked_add(chunk.len() as u64)
            .ok_or("process output cursor overflow")?;
        let count = HEAD.saturating_sub(self.head.len()).min(chunk.len());
        self.head.extend_from_slice(&chunk[..count]);
        let chunk = &chunk[count..];
        let excess = self
            .tail
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(TAIL);
        let discard = excess.min(self.tail.len());
        self.tail.drain(..discard);
        self.tail.extend(&chunk[excess - discard..]);
        Ok(())
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn page(
        &self,
        cursor: u64,
        limit: usize,
        final_output: bool,
    ) -> Result<Page, &'static str> {
        if cursor > self.total {
            return Err("process output cursor is beyond observed output");
        }
        if !(4..=64 * 1024).contains(&limit) {
            return Err("process output page bound is invalid");
        }
        let mut page = Page {
            bytes: Vec::new(),
            omitted: 0,
            omitted_at: None,
            cursor,
        };
        if cursor < self.head.len() as u64 {
            let start =
                usize::try_from(cursor).map_err(|_| "process output cursor exceeds host bounds")?;
            let end = start.saturating_add(limit).min(self.head.len());
            page.bytes.extend_from_slice(&self.head[start..end]);
            page.cursor = end as u64;
        }
        let tail_start = self.total.saturating_sub(self.tail.len() as u64);
        if page.bytes.len() < limit && page.cursor < tail_start {
            page.omitted = tail_start - page.cursor;
            page.omitted_at = Some(page.bytes.len());
            page.cursor = tail_start;
        }
        if page.cursor < tail_start {
            align_page(
                &mut page,
                self.total,
                self.head.len() as u64,
                tail_start,
                final_output,
            );
            return Ok(page);
        }
        let start = usize::try_from(page.cursor - tail_start)
            .map_err(|_| "process output cursor exceeds host bounds")?;
        let count = limit.saturating_sub(page.bytes.len());
        let before = page.bytes.len();
        page.bytes.extend(self.tail.iter().skip(start).take(count));
        page.cursor = page
            .cursor
            .saturating_add((page.bytes.len() - before) as u64);
        align_page(
            &mut page,
            self.total,
            self.head.len() as u64,
            tail_start,
            final_output,
        );
        Ok(page)
    }
}

fn align_page(page: &mut Page, total: u64, head_end: u64, tail_start: u64, final_output: bool) {
    // A named omission may bisect a scalar. Otherwise preserve a partial
    // trailing UTF-8 scalar for the next page or the next process read.
    if (page.cursor == total && final_output) || (page.cursor == head_end && tail_start > head_end)
    {
        return;
    }
    let mut offset = 0;
    while offset < page.bytes.len() {
        match std::str::from_utf8(&page.bytes[offset..]) {
            Ok(_) => break,
            Err(error) => match error.error_len() {
                Some(length) => offset += error.valid_up_to() + length,
                None => {
                    let end = offset + error.valid_up_to();
                    page.cursor -= (page.bytes.len() - end) as u64;
                    page.bytes.truncate(end);
                    break;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_keep_a_stable_prefix_and_recent_tail_and_name_each_gap() -> Result<(), &'static str> {
        let mut output = OutputBuffer::default();
        output.append(&vec![b'a'; HEAD])?;
        output.append(&vec![b'b'; TAIL * 2])?;
        let first = output.page(0, 16, true)?;
        assert_eq!(first.bytes, vec![b'a'; 16]);
        assert_eq!(first.omitted, 0);
        assert_eq!(first.cursor, 16);
        let cross = output.page((HEAD - 8) as u64, 16, true)?;
        assert_eq!(cross.bytes, [vec![b'a'; 8], vec![b'b'; 8]].concat());
        assert_eq!(cross.omitted, TAIL as u64);
        assert_eq!(cross.omitted_at, Some(8));
        assert_eq!(cross.cursor, (HEAD + TAIL + 8) as u64);
        let older = output.page(HEAD as u64 + 3, 8, true)?;
        assert_eq!(older.omitted, TAIL as u64 - 3);
        assert_eq!(older.bytes, vec![b'b'; 8]);
        assert!(output.page(output.total() + 1, 8, true).is_err());
        assert!(output.page(0, 0, true).is_err());
        Ok(())
    }
    #[test]
    fn ordinary_pages_wait_for_and_preserve_complete_utf8_scalars() -> Result<(), &'static str> {
        let mut output = OutputBuffer::default();
        output.append("😀éx".as_bytes())?;
        let first = output.page(0, 5, true)?;
        assert_eq!(
            std::str::from_utf8(&first.bytes).map_err(|_| "UTF-8 split")?,
            "😀"
        );
        assert_eq!(first.cursor, 4);
        let rest = output.page(first.cursor, 5, true)?;
        assert_eq!(
            std::str::from_utf8(&rest.bytes).map_err(|_| "UTF-8 split")?,
            "éx"
        );
        let mut partial = OutputBuffer::default();
        partial.append(&[0xf0, 0x9f])?;
        assert!(partial.page(0, 4, false)?.bytes.is_empty());
        partial.append(&[0x98, 0x80])?;
        assert_eq!(partial.page(0, 4, false)?.bytes, "😀".as_bytes());
        Ok(())
    }
    #[test]
    fn two_byte_scalars_survive_split_writes_and_the_head_tail_seam() -> Result<(), &'static str> {
        let mut output = OutputBuffer::default();
        output.append(&[0xc3])?;
        output.append(&[0xa9, b'x', b'y'])?;
        let page = output.page(0, 4, true)?;
        assert_eq!(
            std::str::from_utf8(&page.bytes).map_err(|_| "UTF-8 split")?,
            "éxy"
        );
        assert_eq!(page.cursor, 4);
        let mut seam = OutputBuffer::default();
        seam.append(&vec![b'a'; HEAD - 1])?;
        seam.append(&[0xc3, 0xa9, b'x', b'x', b'x', b'x'])?;
        let page = seam.page((HEAD - 1) as u64, 8, true)?;
        assert_eq!(
            std::str::from_utf8(&page.bytes).map_err(|_| "UTF-8 split")?,
            "éxxxx"
        );
        assert_eq!(page.omitted, 0);
        assert_eq!(page.cursor, (HEAD + 5) as u64);
        Ok(())
    }

    #[test]
    fn three_byte_scalars_survive_split_writes_and_the_head_tail_seam() -> Result<(), &'static str>
    {
        let mut output = OutputBuffer::default();
        output.append(&[0xe2, 0x82])?;
        output.append(&[0xac, b'x', b'y'])?;
        let page = output.page(0, 5, true)?;
        assert_eq!(
            std::str::from_utf8(&page.bytes).map_err(|_| "UTF-8 split")?,
            "€xy"
        );
        assert_eq!(page.cursor, 5);
        let mut seam = OutputBuffer::default();
        seam.append(&vec![b'a'; HEAD - 2])?;
        seam.append(&[0xe2, 0x82, 0xac, b'x', b'x', b'x', b'x'])?;
        let page = seam.page((HEAD - 2) as u64, 8, true)?;
        assert_eq!(
            std::str::from_utf8(&page.bytes).map_err(|_| "UTF-8 split")?,
            "€xxxx"
        );
        assert_eq!(page.omitted, 0);
        assert_eq!(page.cursor, (HEAD + 5) as u64);
        Ok(())
    }

    #[test]
    fn four_byte_scalars_cross_page_and_seam_boundaries() -> Result<(), &'static str> {
        let mut output = OutputBuffer::default();
        output.append("x😀y".as_bytes())?;
        let first = output.page(0, 5, true)?;
        assert_eq!(
            std::str::from_utf8(&first.bytes).map_err(|_| "UTF-8 split")?,
            "x😀"
        );
        assert_eq!(first.cursor, 5);
        let rest = output.page(first.cursor, 4, true)?;
        assert_eq!(
            std::str::from_utf8(&rest.bytes).map_err(|_| "UTF-8 split")?,
            "y"
        );
        assert_eq!(rest.cursor, 6);
        let mut seam = OutputBuffer::default();
        seam.append(&vec![b'a'; HEAD - 2])?;
        seam.append(&[0xf0, 0x9f, 0x98, 0x80, b'b', b'b'])?;
        let page = seam.page((HEAD - 2) as u64, 8, true)?;
        assert_eq!(
            std::str::from_utf8(&page.bytes).map_err(|_| "UTF-8 split")?,
            "😀bb"
        );
        assert_eq!(page.cursor, (HEAD + 4) as u64);
        let cut = seam.page((HEAD - 5) as u64, 5, true)?;
        assert_eq!(cut.bytes, vec![b'a'; 3]);
        assert_eq!(cut.cursor, (HEAD - 2) as u64);
        let again = seam.page((HEAD - 4) as u64, 10, true)?;
        assert_eq!(
            std::str::from_utf8(&again.bytes).map_err(|_| "UTF-8 split")?,
            "aa😀bb"
        );
        assert_eq!(again.cursor, (HEAD + 4) as u64);
        Ok(())
    }

    #[test]
    fn retention_gaps_name_omitted_bytes_when_they_bisect_scalars() -> Result<(), &'static str> {
        let mut output = OutputBuffer::default();
        output.append(&vec![b'a'; HEAD - 2])?;
        output.append(&[0xf0, 0x9f])?;
        output.append(&vec![b'b'; TAIL + 10])?;
        assert_eq!(output.total() as usize, HEAD + TAIL + 10);
        let page = output.page((HEAD - 2) as u64, 16, true)?;
        assert_eq!(page.omitted, 10);
        assert_eq!(page.omitted_at, Some(2));
        assert_eq!(page.bytes.len(), 16);
        assert_eq!(&page.bytes[..2], &[0xf0, 0x9f]);
        assert_eq!(&page.bytes[2..], vec![b'b'; 14]);
        assert_eq!(page.cursor, (HEAD + 24) as u64);
        Ok(())
    }

    #[test]
    fn evicted_scalar_leaders_leave_continuations_honest_at_tail_start() -> Result<(), &'static str>
    {
        let mut output = OutputBuffer::default();
        output.append(&vec![b'a'; HEAD])?;
        output.append(&vec![b'b'; TAIL + 2])?;
        output.append(&[0xf0, 0x9f, 0x98, 0x80])?;
        output.append(&vec![b'c'; TAIL - 2])?;
        let start = (HEAD + TAIL + 4) as u64;
        let page = output.page(start, 8, true)?;
        assert_eq!(page.omitted, 0);
        assert_eq!(page.omitted_at, None);
        assert_eq!(&page.bytes[..2], &[0x98, 0x80]);
        assert_eq!(&page.bytes[2..], vec![b'c'; 6]);
        assert_eq!(page.cursor, start + 8);
        let next = output.page(page.cursor, 4, true)?;
        assert_eq!(next.bytes, vec![b'c'; 4]);
        assert_eq!(next.cursor, start + 12);
        Ok(())
    }

    #[test]
    fn final_output_keeps_raw_incomplete_bytes_until_the_stream_completes(
    ) -> Result<(), &'static str> {
        let mut output = OutputBuffer::default();
        output.append(b"z")?;
        output.append(&[0xf0, 0x9f])?;
        let settled = output.page(0, 8, true)?;
        assert_eq!(settled.bytes, vec![b'z', 0xf0, 0x9f]);
        assert_eq!(settled.cursor, output.total());
        let waiting = output.page(0, 8, false)?;
        assert_eq!(waiting.bytes, vec![b'z']);
        assert_eq!(waiting.cursor, 1);
        output.append(&[0x98, 0x80])?;
        let done = output.page(0, 8, true)?;
        assert_eq!(
            std::str::from_utf8(&done.bytes).map_err(|_| "UTF-8 split")?,
            "z😀"
        );
        assert_eq!(done.cursor, output.total());
        Ok(())
    }

    #[test]
    fn cursors_into_scalar_middles_account_isolated_continuations() -> Result<(), &'static str> {
        let mut output = OutputBuffer::default();
        output.append("x😀y".as_bytes())?;
        let mid = output.page(2, 4, true)?;
        assert_eq!(mid.bytes, vec![0x9f, 0x98, 0x80, b'y']);
        assert_eq!(mid.cursor, 6);
        assert_eq!(mid.omitted, 0);
        let rest = output.page(mid.cursor, 4, true)?;
        assert!(rest.bytes.is_empty());
        assert_eq!(rest.cursor, 6);
        Ok(())
    }
}
