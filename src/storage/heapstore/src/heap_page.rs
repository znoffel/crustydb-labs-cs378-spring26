use crate::page;
use crate::page::{Offset, Page};
use common::prelude::*;
use common::PAGE_SIZE;
use std::fmt;
use std::fmt::Write;

///byte length of a slot value distinct from Offset which is a position
pub type SlotLength = u16;

//page header field byte offsets
///num_slots byte offset in the header
const PAGE_META_NUM_SLOTS_OFFSET: usize = 2;
///free_start byte offset in the header
const PAGE_META_FREE_START_OFFSET: usize = 4;
///reserved padding byte offset in the header
const PAGE_META_RESERVED_OFFSET: usize = 6;
///size of the fixed page metadata block
const FIXED_PAGE_META_SIZE: usize = 8;
///size of one slot metadata entry
const BYTES_PER_SLOT_META: usize = 6;

//slot entry field offsets relative to slot entry start
///record page offset within a slot entry
const SLOT_OFFSET_OFFSET: usize = 0;
///record byte length within a slot entry
const SLOT_LENGTH_OFFSET: usize = 2;
///in_use flag offset within a slot entry
const SLOT_IN_USE_OFFSET: usize = 4;

///slot holds a live record
const SLOT_IN_USE_VALID: u8 = 1;
///slot is free or deleted
const SLOT_IN_USE_FREE: u8 = 0;

pub trait HeapPage {
    fn add_value(&mut self, bytes: &[u8]) -> Option<SlotId>;
    fn get_value(&self, slot_id: SlotId) -> Option<Vec<u8>>;
    fn delete_value(&mut self, slot_id: SlotId) -> Option<()>;
    fn get_header_size(&self) -> usize;
    fn get_free_space(&self) -> usize;
}

impl HeapPage for Page {
    ///total header size including all slot entries
    fn get_header_size(&self) -> usize {
        FIXED_PAGE_META_SIZE + self.get_num_slots() * BYTES_PER_SLOT_META
    }

    ///free bytes remaining
    fn get_free_space(&self) -> usize {
        let header_size = self.get_header_size();
        let used_bytes: usize = self
            .iter_used_slots()
            .map(|(_, len)| len as usize)
            .sum::<usize>();
        PAGE_SIZE.saturating_sub(header_size).saturating_sub(used_bytes)
    }

    ///inserts bytes and returns the assigned SlotId or None if no space
    ///always reuses the lowest free SlotId
    fn add_value(&mut self, bytes: &[u8]) -> Option<SlotId> {
        let value_len = bytes.len();
        if value_len > PAGE_SIZE {
            return None;
        }
    
        let slot_id = self.find_lowest_free_slot_id();
        let num_slots = self.get_num_slots();
        let need_new_slot = (slot_id as usize) >= num_slots;
    
        let extra_header = if need_new_slot { BYTES_PER_SLOT_META } else { 0 };
        if self.get_free_space() < value_len + extra_header {
            return None;
        }
    
        //compact before growing the header so free_start is accurate for the shift
        let free_start = self.get_free_start();
        let contiguous_space = PAGE_SIZE.saturating_sub(free_start + extra_header);
        if contiguous_space < value_len {
            self.compact();
        }
    
        if need_new_slot {
            if num_slots > 0 {
                self.shift_body_for_new_slot();
            }
            self.set_num_slots(num_slots + 1);
        }
    
        let insert_offset = self.get_free_start();
        if insert_offset + value_len > PAGE_SIZE {
            return None;
        }
    
        self.data[insert_offset..insert_offset + value_len].clone_from_slice(bytes);
        self.write_slot(slot_id, insert_offset as Offset, value_len as SlotLength, SLOT_IN_USE_VALID);
        self.set_free_start(insert_offset + value_len);
    
        Some(slot_id)
    }

    ///record bytes for slot_id or None if invalid or deleted
    fn get_value(&self, slot_id: SlotId) -> Option<Vec<u8>> {
        if self.get_slot_in_use(slot_id)? != SLOT_IN_USE_VALID {
            return None;
        }
        let (offset, length) = self.get_slot_offset_length(slot_id)?;
        let offset = offset as usize;
        let length = length as usize;
        if offset + length > PAGE_SIZE {
            return None;
        }
        Some(self.data[offset..offset + length].to_vec())
    }

    ///marks slot as free or None if out of range or already deleted
    fn delete_value(&mut self, slot_id: SlotId) -> Option<()> {
        if (slot_id as usize) >= self.get_num_slots() {
            return None;
        }
        if self.get_slot_in_use(slot_id)? != SLOT_IN_USE_VALID {
            return None;
        }
        self.set_slot_in_use(slot_id, SLOT_IN_USE_FREE);
        Some(())
    }
}

//private helper methods
impl Page {
    ///number of slot entries in the header
    fn get_num_slots(&self) -> usize {
        u16::from_le_bytes(
            self.data[PAGE_META_NUM_SLOTS_OFFSET..PAGE_META_NUM_SLOTS_OFFSET + 2]
                .try_into()
                .unwrap(),
        ) as usize
    }

    ///writes num_slots to the header
    fn set_num_slots(&mut self, n: usize) {
        self.data[PAGE_META_NUM_SLOTS_OFFSET..PAGE_META_NUM_SLOTS_OFFSET + 2]
            .copy_from_slice(&(n as u16).to_le_bytes());
    }

    ///first free body byte clamps to body_start if the stored value is stale
    fn get_free_start(&self) -> usize {
        let num_slots = self.get_num_slots();
        let body_start = FIXED_PAGE_META_SIZE + num_slots * BYTES_PER_SLOT_META;
        let stored = Offset::from_le_bytes(
            self.data[PAGE_META_FREE_START_OFFSET..PAGE_META_FREE_START_OFFSET + 2]
                .try_into()
                .unwrap(),
        ) as usize;
        let raw = if stored < body_start { body_start } else { stored };
        raw.min(PAGE_SIZE)
    }

    ///writes free_start to the header clamped to PAGE_SIZE
    fn set_free_start(&mut self, pos: usize) {
        let pos = pos.min(PAGE_SIZE);
        self.data[PAGE_META_FREE_START_OFFSET..PAGE_META_FREE_START_OFFSET + 2]
            .copy_from_slice(&(pos as Offset).to_le_bytes());
    }

    ///byte offset of slot_id metadata entry in data
    fn slot_meta_offset(&self, slot_id: SlotId) -> usize {
        FIXED_PAGE_META_SIZE + (slot_id as usize) * BYTES_PER_SLOT_META
    }

    ///offset and length for slot_id or None if out of range
    fn get_slot_offset_length(&self, slot_id: SlotId) -> Option<(Offset, SlotLength)> {
        if slot_id as usize >= self.get_num_slots() {
            return None;
        }
        let base = self.slot_meta_offset(slot_id);
        let offset = Offset::from_le_bytes(
            self.data[base + SLOT_OFFSET_OFFSET..base + SLOT_OFFSET_OFFSET + 2]
                .try_into()
                .unwrap(),
        );
        let length = SlotLength::from_le_bytes(
            self.data[base + SLOT_LENGTH_OFFSET..base + SLOT_LENGTH_OFFSET + 2]
                .try_into()
                .unwrap(),
        );
        Some((offset, length))
    }

    ///in_use flag for slot_id or None if out of range
    fn get_slot_in_use(&self, slot_id: SlotId) -> Option<u8> {
        if slot_id as usize >= self.get_num_slots() {
            return None;
        }
        let base = self.slot_meta_offset(slot_id);
        Some(self.data[base + SLOT_IN_USE_OFFSET])
    }

    ///sets in_use for slot_id
    fn set_slot_in_use(&mut self, slot_id: SlotId, in_use: u8) {
        let base = self.slot_meta_offset(slot_id);
        self.data[base + SLOT_IN_USE_OFFSET] = in_use;
    }

    ///writes offset and length and in_use into slot_id metadata
    fn write_slot(&mut self, slot_id: SlotId, offset: Offset, length: SlotLength, in_use: u8) {
        let base = self.slot_meta_offset(slot_id);
        self.data[base..base + 2].copy_from_slice(&offset.to_le_bytes());
        self.data[base + 2..base + 4].copy_from_slice(&length.to_le_bytes());
        self.data[base + SLOT_IN_USE_OFFSET] = in_use;
    }

    ///lowest free SlotId or num_slots if all in use
    fn find_lowest_free_slot_id(&self) -> SlotId {
        let num_slots = self.get_num_slots();
        for slot_id in 0..num_slots {
            if self
                .get_slot_in_use(slot_id as SlotId)
                .map_or(true, |u| u == SLOT_IN_USE_FREE)
            {
                return slot_id as SlotId;
            }
        }
        num_slots as SlotId
    }

    ///slot_id and length for every live slot
    fn iter_used_slots(&self) -> impl Iterator<Item = (SlotId, SlotLength)> + '_ {
        let num_slots = self.get_num_slots();
        (0..num_slots).filter_map(move |i| {
            let sid = i as SlotId;
            if self
                .get_slot_in_use(sid)
                .map_or(false, |u| u == SLOT_IN_USE_VALID)
            {
                self.get_slot_offset_length(sid).map(|(_, len)| (sid, len))
            } else {
                None
            }
        })
    }

    ///moves all live records to body start and resets free_start
    fn compact(&mut self) {
        let num_slots = self.get_num_slots();
        let body_start = FIXED_PAGE_META_SIZE + num_slots * BYTES_PER_SLOT_META;

        //sort by offset so copies never overlap
        let mut used: Vec<(SlotId, usize, usize)> = (0..num_slots)
            .filter_map(|i| {
                let sid = i as SlotId;
                if self
                    .get_slot_in_use(sid)
                    .map_or(false, |u| u == SLOT_IN_USE_VALID)
                {
                    self.get_slot_offset_length(sid)
                        .map(|(off, len)| (sid, off as usize, len as usize))
                } else {
                    None
                }
            })
            .collect();
        used.sort_by_key(|&(_, off, _)| off);

        let mut write_pos = body_start;
        for (slot_id, old_offset, length) in used {
            if old_offset != write_pos {
                self.data.copy_within(old_offset..old_offset + length, write_pos);
            }
            self.write_slot(slot_id, write_pos as Offset, length as SlotLength, SLOT_IN_USE_VALID);
            write_pos += length;
        }
        self.set_free_start(write_pos);
    }

    ///shifts body right by BYTES_PER_SLOT_META for a new slot entry
    ///bumps all existing slot offsets to match
    fn shift_body_for_new_slot(&mut self) {
        let num_slots = self.get_num_slots();
        let old_body_start = FIXED_PAGE_META_SIZE + num_slots * BYTES_PER_SLOT_META;
        let new_body_start = old_body_start + BYTES_PER_SLOT_META;

        let free_start = self.get_free_start();
        let body_len = free_start.saturating_sub(old_body_start);

        if body_len > 0 {
            let shift_len = body_len.min(PAGE_SIZE - new_body_start);
            self.data
                .copy_within(old_body_start..old_body_start + shift_len, new_body_start);
        }

        //zero stale bytes now occupied by the new slot entry
        self.data[old_body_start..new_body_start].fill(0);

        for slot_id in 0..num_slots {
            let sid = slot_id as SlotId;
            if self
                .get_slot_in_use(sid)
                .map_or(false, |u| u == SLOT_IN_USE_VALID)
            {
                if let Some((off, len)) = self.get_slot_offset_length(sid) {
                    self.write_slot(
                        sid,
                        (off as usize + BYTES_PER_SLOT_META) as Offset,
                        len,
                        SLOT_IN_USE_VALID,
                    );
                }
            }
        }

        let new_free = (free_start + BYTES_PER_SLOT_META).min(PAGE_SIZE);
        self.set_free_start(new_free);
    }
}

///consuming iterator over valid records in ascending SlotId order
pub struct HeapPageIntoIter {
    page: Page,
    current_slot: SlotId,
    num_slots: usize,
}

impl Iterator for HeapPageIntoIter {
    type Item = (Vec<u8>, SlotId);

    fn next(&mut self) -> Option<Self::Item> {
        while (self.current_slot as usize) < self.num_slots {
            let slot_id = self.current_slot;
            self.current_slot += 1;
            if let Some(value) = self.page.get_value(slot_id) {
                return Some((value, slot_id));
            }
        }
        None
    }
}

///creates a consuming iterator from a page
impl IntoIterator for Page {
    type Item = (Vec<u8>, SlotId);
    type IntoIter = HeapPageIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        let num_slots = self.get_num_slots();
        HeapPageIntoIter {
            page: self,
            current_slot: 0,
            num_slots,
        }
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
    pub const FIXED_HEADER_SIZE: usize = 8;
    pub const HEADER_PER_VAL_SIZE: usize = 6;

    #[test]
    fn hs_page_sizes_header_free_space() {
        init();
        let p = Page::new(0);
        assert_eq!(0, p.get_page_id());

        assert_eq!(PAGE_SIZE - p.get_header_size(), p.get_free_space());
    }

    #[test]
    fn hs_page_debug_insert() {
        init();
        let mut p = Page::new(0);
        let n = 20;
        let size = 20;
        let vals = get_ascending_vec_of_byte_vec_02x(n, size, size);
        for x in &vals {
            p.add_value(x);
        }
        assert_eq!(
            p.get_free_space(),
            PAGE_SIZE - p.get_header_size() - n * size
        );
    }

    #[test]
    fn hs_page_simple_insert() {
        init();
        let mut p = Page::new(5);
        let tuple = int_vec_to_tuple(vec![0, 1, 2]);
        let tuple_bytes = serde_cbor::to_vec(&tuple).unwrap();
        let byte_len = tuple_bytes.len();
        assert_eq!(Some(0), p.add_value(&tuple_bytes));
        assert_eq!(
            PAGE_SIZE - byte_len - p.get_header_size(),
            p.get_free_space()
        );
        let tuple_bytes2 = serde_cbor::to_vec(&tuple).unwrap();
        assert_eq!(Some(1), p.add_value(&tuple_bytes2));
        assert_eq!(
            PAGE_SIZE - p.get_header_size() - byte_len - byte_len,
            p.get_free_space()
        );
    }

    #[test]
    fn hs_page_space() {
        init();
        let mut p = Page::new(0);
        let size = 10;
        let bytes = get_random_byte_vec(size);
        assert_eq!(10, bytes.len());
        assert_eq!(Some(0), p.add_value(&bytes));
        assert_eq!(PAGE_SIZE - p.get_header_size() - size, p.get_free_space());
        assert_eq!(Some(1), p.add_value(&bytes));
        assert_eq!(
            PAGE_SIZE - p.get_header_size() - size * 2,
            p.get_free_space()
        );
        assert_eq!(Some(2), p.add_value(&bytes));
        assert_eq!(
            PAGE_SIZE - p.get_header_size() - size * 3,
            p.get_free_space()
        );
    }

    #[test]
    fn hs_page_get_value() {
        init();
        let mut p = Page::new(0);
        let tuple = int_vec_to_tuple(vec![0, 1, 2]);
        let tuple_bytes = serde_cbor::to_vec(&tuple).unwrap();
        assert_eq!(Some(0), p.add_value(&tuple_bytes));
        let check_bytes = p.get_value(0).unwrap();
        let check_tuple: Tuple = serde_cbor::from_slice(&check_bytes).unwrap();
        assert_eq!(tuple_bytes, check_bytes);
        assert_eq!(tuple, check_tuple);

        let tuple2 = int_vec_to_tuple(vec![3, 3, 3]);
        let tuple_bytes2 = serde_cbor::to_vec(&tuple2).unwrap();
        assert_eq!(Some(1), p.add_value(&tuple_bytes2));
        let check_bytes2 = p.get_value(1).unwrap();
        let check_tuple2: Tuple = serde_cbor::from_slice(&check_bytes2).unwrap();
        assert_eq!(tuple_bytes2, check_bytes2);
        assert_eq!(tuple2, check_tuple2);

        //Recheck
        let check_bytes2 = p.get_value(1).unwrap();
        let check_tuple2: Tuple = serde_cbor::from_slice(&check_bytes2).unwrap();
        assert_eq!(tuple_bytes2, check_bytes2);
        assert_eq!(tuple2, check_tuple2);
        let check_bytes = p.get_value(0).unwrap();
        let check_tuple: Tuple = serde_cbor::from_slice(&check_bytes).unwrap();
        assert_eq!(tuple_bytes, check_bytes);
        assert_eq!(tuple, check_tuple);

        //Check that invalid slot gets None
        assert_eq!(None, p.get_value(2));
    }

    #[test]
    fn hs_page_header_size_small() {
        init();
        // Testing that the header is no more than 8 bytes for the header, and 6 bytes per value inserted
        let mut p = Page::new(0);
        assert!(p.get_header_size() <= FIXED_HEADER_SIZE);
        let bytes = get_random_byte_vec(10);
        assert_eq!(Some(0), p.add_value(&bytes));
        assert!(p.get_header_size() <= FIXED_HEADER_SIZE + HEADER_PER_VAL_SIZE);
        assert_eq!(Some(1), p.add_value(&bytes));
        assert_eq!(Some(2), p.add_value(&bytes));
        assert_eq!(Some(3), p.add_value(&bytes));
        assert!(p.get_header_size() <= FIXED_HEADER_SIZE + HEADER_PER_VAL_SIZE * 4);
    }

    #[test]
    fn hs_page_header_size_full() {
        init();
        // Testing that the header is no more than 8 bytes for the header, and 6 bytes per value inserted
        let mut p = Page::new(0);
        assert!(p.get_header_size() <= FIXED_HEADER_SIZE);
        let byte_size = 10;
        let bytes = get_random_byte_vec(byte_size);
        // how many vals can we hold with 8 bytes
        let num_vals: usize = (((PAGE_SIZE - FIXED_HEADER_SIZE) as f64
            / (byte_size + HEADER_PER_VAL_SIZE) as f64)
            .floor()) as usize;
        if PAGE_SIZE == 4096 && FIXED_HEADER_SIZE == 8 && HEADER_PER_VAL_SIZE == 6 {
            assert_eq!(255, num_vals);
        }
        for _ in 0..num_vals {
            p.add_value(&bytes);
        }
        assert!(p.get_header_size() <= FIXED_HEADER_SIZE + (num_vals * HEADER_PER_VAL_SIZE));
        assert!(
            p.get_free_space()
                >= PAGE_SIZE
                    - (byte_size * num_vals)
                    - FIXED_HEADER_SIZE
                    - (num_vals * HEADER_PER_VAL_SIZE)
        );
    }

    #[test]
    fn hs_page_no_space() {
        init();
        let mut p = Page::new(0);
        let size = PAGE_SIZE / 4;
        let bytes = get_random_byte_vec(size);
        assert_eq!(Some(0), p.add_value(&bytes));
        assert_eq!(PAGE_SIZE - p.get_header_size() - size, p.get_free_space());
        assert_eq!(Some(1), p.add_value(&bytes));
        assert_eq!(
            PAGE_SIZE - p.get_header_size() - size * 2,
            p.get_free_space()
        );
        assert_eq!(Some(2), p.add_value(&bytes));
        assert_eq!(
            PAGE_SIZE - p.get_header_size() - size * 3,
            p.get_free_space()
        );
        //Should reject here
        assert_eq!(None, p.add_value(&bytes));
        assert_eq!(
            PAGE_SIZE - p.get_header_size() - size * 3,
            p.get_free_space()
        );
        // Take small amount of data
        let small_bytes = get_random_byte_vec(size / 4);
        assert_eq!(Some(3), p.add_value(&small_bytes));
        assert_eq!(
            PAGE_SIZE - p.get_header_size() - size * 3 - small_bytes.len(),
            p.get_free_space()
        );
    }

    #[test]
    fn hs_page_simple_delete() {
        init();
        let mut p = Page::new(0);
        let tuple = int_vec_to_tuple(vec![0, 1, 2]);
        let tuple_bytes = serde_cbor::to_vec(&tuple).unwrap();
        assert_eq!(Some(0), p.add_value(&tuple_bytes));
        let check_bytes = p.get_value(0).unwrap();
        let check_tuple: Tuple = serde_cbor::from_slice(&check_bytes).unwrap();
        assert_eq!(tuple_bytes, check_bytes);
        assert_eq!(tuple, check_tuple);

        let tuple2 = int_vec_to_tuple(vec![3, 3, 3]);
        let tuple_bytes2 = serde_cbor::to_vec(&tuple2).unwrap();
        assert_eq!(Some(1), p.add_value(&tuple_bytes2));
        let check_bytes2 = p.get_value(1).unwrap();
        let check_tuple2: Tuple = serde_cbor::from_slice(&check_bytes2).unwrap();
        assert_eq!(tuple_bytes2, check_bytes2);
        assert_eq!(tuple2, check_tuple2);

        //Delete slot 0
        assert_eq!(Some(()), p.delete_value(0));

        //Recheck slot 1
        let check_bytes2 = p.get_value(1).unwrap();
        let check_tuple2: Tuple = serde_cbor::from_slice(&check_bytes2).unwrap();
        assert_eq!(tuple_bytes2, check_bytes2);
        assert_eq!(tuple2, check_tuple2);

        //Verify slot 0 is gone
        assert_eq!(None, p.get_value(0));

        //Check that invalid slot gets None
        assert_eq!(None, p.get_value(2));

        //Delete slot 1
        assert_eq!(Some(()), p.delete_value(1));

        //Verify slot 0 is gone
        assert_eq!(None, p.get_value(1));
    }

    #[test]
    fn hs_page_delete_insert() {
        init();
        let mut p = Page::new(0);
        let tuple_bytes = get_random_byte_vec(20);
        let tuple_bytes2 = get_random_byte_vec(20);
        let tuple_bytes3 = get_random_byte_vec(20);
        let tuple_bytes4 = get_random_byte_vec(20);
        let tuple_bytes_big = get_random_byte_vec(40);
        let tuple_bytes_small1 = get_random_byte_vec(5);
        let tuple_bytes_small2 = get_random_byte_vec(5);

        //Add 3 values
        assert_eq!(Some(0), p.add_value(&tuple_bytes));
        let check_bytes = p.get_value(0).unwrap();
        assert_eq!(tuple_bytes, check_bytes);
        assert_eq!(Some(1), p.add_value(&tuple_bytes2));

        let check_bytes = p.get_value(1).unwrap();
        assert_eq!(tuple_bytes2, check_bytes);
        assert_eq!(Some(2), p.add_value(&tuple_bytes3));

        let check_bytes = p.get_value(2).unwrap();
        assert_eq!(tuple_bytes3, check_bytes);

        //Delete slot 1
        assert_eq!(Some(()), p.delete_value(1));
        //Verify slot 1 is gone
        assert_eq!(None, p.get_value(1));

        let check_bytes = p.get_value(0).unwrap();
        assert_eq!(tuple_bytes, check_bytes);
        let check_bytes = p.get_value(2).unwrap();
        assert_eq!(tuple_bytes3, check_bytes);

        //Insert same bytes, should go to slot 1
        assert_eq!(Some(1), p.add_value(&tuple_bytes4));

        let check_bytes = p.get_value(1).unwrap();
        assert_eq!(tuple_bytes4, check_bytes);

        //Delete 0
        assert_eq!(Some(()), p.delete_value(0));

        //Insert big, should go to slot 0 with space later in free block
        assert_eq!(Some(0), p.add_value(&tuple_bytes_big));

        //Insert small, should go to 3
        assert_eq!(Some(3), p.add_value(&tuple_bytes_small1));

        //Insert small, should go to new
        assert_eq!(Some(4), p.add_value(&tuple_bytes_small2));
    }

    #[test]
    fn hs_page_size() {
        init();
        let mut p = Page::new(2);
        let tuple = int_vec_to_tuple(vec![0, 1, 2]);
        let tuple_bytes = serde_cbor::to_vec(&tuple).unwrap();
        assert_eq!(Some(0), p.add_value(&tuple_bytes));

        let page_bytes = p.to_bytes();
        assert_eq!(PAGE_SIZE, page_bytes.len());
    }

    #[test]
    fn hs_page_simple_byte_serialize() {
        init();
        let mut p = Page::new(0);
        let tuple = int_vec_to_tuple(vec![0, 1, 2]);
        let tuple_bytes = serde_cbor::to_vec(&tuple).unwrap();
        assert_eq!(Some(0), p.add_value(&tuple_bytes));
        let tuple2 = int_vec_to_tuple(vec![3, 3, 3]);
        let tuple_bytes2 = serde_cbor::to_vec(&tuple2).unwrap();
        assert_eq!(Some(1), p.add_value(&tuple_bytes2));

        //Get bytes and create from bytes
        let mut p2 = p.clone();
        assert_eq!(0, p2.get_page_id());

        //Check reads
        let check_bytes2 = p2.get_value(1).unwrap();
        let check_tuple2: Tuple = serde_cbor::from_slice(&check_bytes2).unwrap();
        assert_eq!(tuple_bytes2, check_bytes2);
        assert_eq!(tuple2, check_tuple2);
        let check_bytes = p2.get_value(0).unwrap();
        let check_tuple: Tuple = serde_cbor::from_slice(&check_bytes).unwrap();
        assert_eq!(tuple_bytes, check_bytes);
        assert_eq!(tuple, check_tuple);

        //Add a new tuple to the new page
        let tuple3 = int_vec_to_tuple(vec![4, 3, 2]);
        let tuple_bytes3 = tuple3.to_bytes();
        assert_eq!(Some(2), p2.add_value(&tuple_bytes3));
        assert_eq!(tuple_bytes3, p2.get_value(2).unwrap());
        assert_eq!(tuple_bytes2, p2.get_value(1).unwrap());
        assert_eq!(tuple_bytes, p2.get_value(0).unwrap());
    }

    #[test]
    fn hs_page_serialization_is_deterministic() {
        init();

        // Create a page and serialize it
        let mut p0 = Page::new(0);
        let bytes = get_random_byte_vec(100);
        p0.add_value(&bytes);
        let bytes = get_random_byte_vec(100);
        p0.add_value(&bytes);
        let bytes = get_random_byte_vec(100);
        p0.add_value(&bytes);
        let p0_bytes = p0.to_bytes();

        // Reconstruct the page
        let p1 = Page::from_bytes(*p0_bytes);
        let p1_bytes = p1.to_bytes();

        // Enforce that the two pages serialize determinestically
        assert_eq!(p0_bytes, p1_bytes);
    }

    #[test]
    fn hs_page_iter() {
        init();
        let mut p = Page::new(0);
        let tuple = int_vec_to_tuple(vec![0, 0, 1]);
        let tuple_bytes = serde_cbor::to_vec(&tuple).unwrap();
        assert_eq!(Some(0), p.add_value(&tuple_bytes));

        let tuple2 = int_vec_to_tuple(vec![0, 0, 2]);
        let tuple_bytes2 = serde_cbor::to_vec(&tuple2).unwrap();
        assert_eq!(Some(1), p.add_value(&tuple_bytes2));

        let tuple3 = int_vec_to_tuple(vec![0, 0, 3]);
        let tuple_bytes3 = serde_cbor::to_vec(&tuple3).unwrap();
        assert_eq!(Some(2), p.add_value(&tuple_bytes3));

        let tuple4 = int_vec_to_tuple(vec![0, 0, 4]);
        let tuple_bytes4 = serde_cbor::to_vec(&tuple4).unwrap();
        assert_eq!(Some(3), p.add_value(&tuple_bytes4));

        let tup_vec = vec![
            tuple_bytes.clone(),
            tuple_bytes2.clone(),
            tuple_bytes3.clone(),
            tuple_bytes4.clone(),
        ];
        let page_bytes = *p.to_bytes();

        // Test iteration 1
        let mut iter = p.into_iter();
        assert_eq!(Some((tuple_bytes.clone(), 0)), iter.next());
        assert_eq!(Some((tuple_bytes2.clone(), 1)), iter.next());
        assert_eq!(Some((tuple_bytes3.clone(), 2)), iter.next());
        assert_eq!(Some((tuple_bytes4.clone(), 3)), iter.next());
        assert_eq!(None, iter.next());

        //Check another way
        let p = Page::from_bytes(page_bytes);
        assert_eq!(Some(tuple_bytes.clone()), p.get_value(0));

        for (i, x) in p.into_iter().enumerate() {
            assert_eq!(tup_vec[i], x.0);
        }

        let p = Page::from_bytes(page_bytes);
        let mut count = 0;
        for _ in p {
            count += 1;
        }
        assert_eq!(count, 4);

        //Add a value and check
        let mut p = Page::from_bytes(page_bytes);
        assert_eq!(Some(4), p.add_value(&tuple_bytes));
        //get the updated bytes
        let page_bytes = *p.to_bytes();
        count = 0;
        for _ in p {
            count += 1;
        }
        assert_eq!(count, 5);

        //Delete
        let mut p = Page::from_bytes(page_bytes);
        p.delete_value(2);
        let mut iter = p.into_iter();
        assert_eq!(Some((tuple_bytes.clone(), 0)), iter.next());
        assert_eq!(Some((tuple_bytes2.clone(), 1)), iter.next());
        assert_eq!(Some((tuple_bytes4.clone(), 3)), iter.next());
        assert_eq!(Some((tuple_bytes.clone(), 4)), iter.next());
        assert_eq!(None, iter.next());
    }

    #[test]
    pub fn hs_page_test_delete_reclaim_same_size() {
        init();
        let size = 800;
        let values = get_ascending_vec_of_byte_vec_02x(6, size, size);
        let mut p = Page::new(0);
        assert_eq!(Some(0), p.add_value(&values[0]));
        assert_eq!(Some(1), p.add_value(&values[1]));
        assert_eq!(Some(2), p.add_value(&values[2]));
        assert_eq!(Some(3), p.add_value(&values[3]));
        assert_eq!(Some(4), p.add_value(&values[4]));
        assert_eq!(values[0], p.get_value(0).unwrap());
        assert_eq!(None, p.add_value(&values[0]));
        assert_eq!(Some(()), p.delete_value(1));
        assert_eq!(None, p.get_value(1));
        assert_eq!(Some(1), p.add_value(&values[5]));
        assert_eq!(values[5], p.get_value(1).unwrap());
    }

    #[test]
    pub fn hs_page_test_delete_reclaim_larger_size() {
        init();
        let size = 500;
        let values = get_ascending_vec_of_byte_vec_02x(8, size, size);
        let larger_val = get_random_byte_vec(size * 2 - 20);
        let mut p = Page::new(0);
        assert_eq!(Some(0), p.add_value(&values[0]));
        assert_eq!(Some(1), p.add_value(&values[1]));
        assert_eq!(Some(2), p.add_value(&values[2]));
        assert_eq!(Some(3), p.add_value(&values[3]));
        assert_eq!(Some(4), p.add_value(&values[4]));
        assert_eq!(Some(5), p.add_value(&values[5]));
        assert_eq!(Some(6), p.add_value(&values[6]));
        assert_eq!(Some(7), p.add_value(&values[7]));
        assert_eq!(values[5], p.get_value(5).unwrap());
        assert_eq!(None, p.add_value(&values[0]));
        assert_eq!(Some(()), p.delete_value(1));
        assert_eq!(None, p.get_value(1));
        assert_eq!(Some(()), p.delete_value(6));
        assert_eq!(None, p.get_value(6));
        assert_eq!(Some(1), p.add_value(&larger_val));
        assert_eq!(larger_val, p.get_value(1).unwrap());
    }

    #[test]
    pub fn hs_page_test_delete_reclaim_smaller_size() {
        init();
        let size = 800;
        let values = vec![
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size / 4),
        ];
        let mut p = Page::new(0);
        assert_eq!(Some(0), p.add_value(&values[0]));
        assert_eq!(Some(1), p.add_value(&values[1]));
        assert_eq!(Some(2), p.add_value(&values[2]));
        assert_eq!(Some(3), p.add_value(&values[3]));
        assert_eq!(Some(4), p.add_value(&values[4]));
        assert_eq!(values[0], p.get_value(0).unwrap());
        assert_eq!(None, p.add_value(&values[0]));
        assert_eq!(Some(()), p.delete_value(1));
        assert_eq!(None, p.get_value(1));
        assert_eq!(Some(1), p.add_value(&values[5]));
        assert_eq!(values[5], p.get_value(1).unwrap());
    }

    #[test]
    pub fn hs_page_test_multi_ser() {
        init();
        let size = 500;
        let values = vec![
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size),
            get_random_byte_vec(size),
        ];
        let mut p = Page::new(0);
        assert_eq!(Some(0), p.add_value(&values[0]));
        assert_eq!(Some(1), p.add_value(&values[1]));
        assert_eq!(Some(2), p.add_value(&values[2]));
        let mut p2 = p.clone();
        assert_eq!(values[0], p2.get_value(0).unwrap());
        assert_eq!(values[1], p2.get_value(1).unwrap());
        assert_eq!(values[2], p2.get_value(2).unwrap());
        assert_eq!(Some(3), p2.add_value(&values[3]));
        assert_eq!(Some(4), p2.add_value(&values[4]));

        let mut p3 = p2.clone();
        assert_eq!(values[0], p3.get_value(0).unwrap());
        assert_eq!(values[1], p3.get_value(1).unwrap());
        assert_eq!(values[2], p3.get_value(2).unwrap());
        assert_eq!(values[3], p3.get_value(3).unwrap());
        assert_eq!(values[4], p3.get_value(4).unwrap());
        assert_eq!(Some(5), p3.add_value(&values[5]));
        assert_eq!(Some(6), p3.add_value(&values[6]));
        assert_eq!(Some(7), p3.add_value(&values[7]));
        assert_eq!(None, p3.add_value(&values[0]));

        let p4 = p3.clone();
        assert_eq!(values[0], p4.get_value(0).unwrap());
        assert_eq!(values[1], p4.get_value(1).unwrap());
        assert_eq!(values[2], p4.get_value(2).unwrap());
        assert_eq!(values[7], p4.get_value(7).unwrap());
    }

    #[test]
    pub fn hs_page_stress_test() {
        init();
        let mut p = Page::new(23);
        let mut original_vals: VecDeque<Vec<u8>> =
            VecDeque::from_iter(get_ascending_vec_of_byte_vec_02x(300, 20, 100));
        let mut stored_vals: Vec<Vec<u8>> = Vec::new();
        let mut stored_slots: Vec<SlotId> = Vec::new();
        let mut has_space = true;
        let mut rng = rand::thread_rng();
        // Load up page until full
        while has_space {
            let bytes = original_vals
                .pop_front()
                .expect("ran out of data -- shouldn't happen");
            let slot = p.add_value(&bytes);
            match slot {
                Some(slot_id) => {
                    stored_vals.push(bytes);
                    stored_slots.push(slot_id);
                }
                None => {
                    // No space for this record, we are done. go ahead and stop. add back value
                    original_vals.push_front(bytes);
                    has_space = false;
                }
            };
        }
        // let (check_vals, check_slots): (Vec<Vec<u8>>, Vec<SlotId>) = p.into_iter().map(|(a, b)| (a, b)).unzip();
        let p_clone = p.clone();
        let mut check_vals: Vec<Vec<u8>> = p_clone.into_iter().map(|(a, _)| a).collect();
        assert!(compare_unordered_byte_vecs(&stored_vals, check_vals));
        trace!("\n==================\n PAGE LOADED - now going to delete to make room as needed \n =======================");
        // Delete and add remaining values until goes through all. Should result in a lot of random deletes and adds.
        while !original_vals.is_empty() {
            let bytes = original_vals.pop_front().unwrap();
            trace!("Adding new value (left:{}). Need to make space for new record (len:{}).\n - Stored_slots {:?}", original_vals.len(), &bytes.len(), stored_slots);
            let mut added = false;
            while !added {
                let try_slot = p.add_value(&bytes);
                match try_slot {
                    Some(new_slot) => {
                        stored_slots.push(new_slot);
                        stored_vals.push(bytes.clone());
                        let p_clone = p.clone();
                        check_vals = p_clone.into_iter().map(|(a, _)| a).collect();
                        assert!(compare_unordered_byte_vecs(&stored_vals, check_vals));
                        trace!("Added new value ({}) {:?}", new_slot, stored_slots);
                        added = true;
                    }
                    None => {
                        //Delete a random value and try again
                        let random_idx = rng.gen_range(0..stored_slots.len());
                        trace!(
                            "Deleting a random val to see if that makes enough space {}",
                            stored_slots[random_idx]
                        );
                        let value_id_to_del = stored_slots.remove(random_idx);
                        stored_vals.remove(random_idx);
                        p.delete_value(value_id_to_del)
                            .expect("Error deleting slot_id");
                        trace!("Stored vals left {}", stored_slots.len());
                    }
                }
            }
        }
    }
}
