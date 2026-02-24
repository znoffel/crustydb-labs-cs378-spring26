pub use crate::heap_page::HeapPage;
use common::prelude::*;
use common::PAGE_SIZE;
use std::fmt;
use std::fmt::Write;

///page offset u16 fits any offset in a 4096 byte page cast to usize before indexing
pub type Offset = u16;
//for debug formatting
const BYTES_PER_LINE: usize = 40;

///initial num_slots for a new page
const INITIAL_NUM_SLOTS: u16 = 0;
///initial free_start body begins after the 8 byte page metadata
const INITIAL_FREE_START: Offset = 8;

///fixed size page with 8 bytes metadata and 6 bytes per slot
pub struct Page {
    ///raw page bytes
    pub(crate) data: [u8; PAGE_SIZE],
}

impl Page {
    ///new empty page with the given page_id
    pub fn new(page_id: PageId) -> Self {
        let mut data = [0u8; PAGE_SIZE];
        data[0..2].copy_from_slice(&page_id.to_le_bytes());
        data[2..4].copy_from_slice(&INITIAL_NUM_SLOTS.to_le_bytes());
        data[4..6].copy_from_slice(&INITIAL_FREE_START.to_le_bytes());
        Page { data }
    }

    ///page ID
    pub fn get_page_id(&self) -> PageId {
        PageId::from_le_bytes(self.data[0..2].try_into().unwrap())
    }

    ///page from a raw byte array
    #[allow(dead_code)]
    pub fn from_bytes(data: [u8; PAGE_SIZE]) -> Self {
        Page { data }
    }

    ///reference to the page's raw bytes
    pub fn to_bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.data
    }

    ///list of offsets and differing bytes where this page differs from other_page
    #[allow(dead_code)]
    pub fn compare_page(&self, other_page: Vec<u8>) -> Vec<(Offset, Vec<u8>)> {
        let mut res = Vec::new();
        let bytes = self.to_bytes();
        assert_eq!(bytes.len(), other_page.len());
        let mut in_diff = false;
        let mut diff_start = 0;
        let mut diff_vec: Vec<u8> = Vec::new();
        for (i, (b1, b2)) in bytes.iter().zip(&other_page).enumerate() {
            if b1 != b2 {
                if !in_diff {
                    diff_start = i;
                    in_diff = true;
                }
                diff_vec.push(*b1);
            } else if in_diff {
                //end diff
                res.push((diff_start as Offset, diff_vec.clone()));
                diff_vec.clear();
                in_diff = false;
            }
        }
        res
    }
}

impl Clone for Page {
    fn clone(&self) -> Self {
        Page { data: self.data }
    }
}

impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        //let bytes: &[u8] = unsafe { any_as_u8_slice(&self) };
        let p = self.to_bytes();
        let mut buffer = String::new();
        let len_bytes = p.len();

        writeln!(&mut buffer, "PID:{}", self.get_page_id()).unwrap();
        let mut pos = 0;
        let mut remaining;
        let mut empty_lines_count = 0;
        let comp = [0; BYTES_PER_LINE];
        //hide the empty lines
        while pos < len_bytes {
            remaining = len_bytes - pos;
            if remaining > BYTES_PER_LINE {
                let pv = &(p)[pos..pos + BYTES_PER_LINE];
                if pv.eq(&comp) {
                    empty_lines_count += 1;
                    pos += BYTES_PER_LINE;
                    continue;
                }
                if empty_lines_count != 0 {
                    write!(&mut buffer, "{} ", empty_lines_count).unwrap();
                    buffer += "empty lines were hidden\n";
                    empty_lines_count = 0;
                }
                // for hex offset
                write!(&mut buffer, "[{:4}] ", pos).unwrap();
                #[allow(clippy::needless_range_loop)]
                for i in 0..BYTES_PER_LINE {
                    match pv[i] {
                        0x00 => buffer += ".  ",
                        0xff => buffer += "## ",
                        _ => write!(&mut buffer, "{:02x} ", pv[i]).unwrap(),
                    };
                }
            } else {
                let pv = &(*p)[pos..pos + remaining];
                if pv.eq(&comp) {
                    empty_lines_count += 1;
                    pos += BYTES_PER_LINE;
                    continue;
                }
                if empty_lines_count != 0 {
                    write!(&mut buffer, "{} ", empty_lines_count).unwrap();
                    buffer += "empty lines were hidden\n";
                    empty_lines_count = 0;
                }
                // for hex offset
                //buffer += &format!("[0x{:08x}] ", pos);
                write!(&mut buffer, "[{:4}] ", pos).unwrap();
                #[allow(clippy::needless_range_loop)]
                for i in 0..remaining {
                    match pv[i] {
                        0x00 => buffer += ".  ",
                        0xff => buffer += "## ",
                        _ => write!(&mut buffer, "{:02x} ", pv[i]).unwrap(),
                    };
                }
            }
            buffer += "\n";
            pos += BYTES_PER_LINE;
        }
        if empty_lines_count != 0 {
            write!(&mut buffer, "{} ", empty_lines_count).unwrap();
            buffer += "empty lines were hidden\n";
        }
        write!(f, "{}", buffer)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use common::testutil::init;
    use common::testutil::*;
    use common::Tuple;
    use rand::Rng;

    /// Limits how on how many bytes we can use for page metadata / header

    #[test]
    fn hs_page_create_basic() {
        init();
        let p = Page::new(0);
        assert_eq!(0, p.get_page_id());

        let p = Page::new(1);
        assert_eq!(1, p.get_page_id());

        let p = Page::new(1023);
        assert_eq!(1023, p.get_page_id());
    }
}
