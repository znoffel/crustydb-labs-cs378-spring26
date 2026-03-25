use crate::heap_page::HeapPageIntoIter;
use crate::heapfile::HeapFile;
use crate::page::Page;
use common::prelude::*;
use std::sync::Arc;

pub struct HeapFileIterator {
    hf: Arc<HeapFile>,
    current_page_id: PageId,
    start_page_id: PageId,
    start_slot_id: Option<SlotId>,
    current_page_iter: Option<HeapPageIntoIter>,
}

impl HeapFileIterator {
    pub(crate) fn new(_tid: TransactionId, hf: Arc<HeapFile>) -> Self {
        HeapFileIterator {
            hf,
            current_page_id: 0,
            start_page_id: 0,
            start_slot_id: None,
            current_page_iter: None,
        }
    }

    pub(crate) fn new_from(_tid: TransactionId, hf: Arc<HeapFile>, value_id: ValueId) -> Self {
        let start_page = value_id.page_id.unwrap_or(0);
        HeapFileIterator {
            hf,
            current_page_id: start_page,
            start_page_id: start_page,
            start_slot_id: value_id.slot_id,
            current_page_iter: None,
        }
    }
}

impl Iterator for HeapFileIterator {
    type Item = (Vec<u8>, ValueId);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let item = self.current_page_iter.as_mut().and_then(|iter| iter.next());
            match item {
                Some((bytes, slot_id)) => {
                    let page_id = self.current_page_id - 1;
                    if page_id == self.start_page_id {
                        if let Some(start_slot) = self.start_slot_id {
                            if slot_id < start_slot {
                                continue;
                            }
                        }
                    }
                    let vid = ValueId::new_slot(self.hf.container_id, page_id, slot_id);
                    return Some((bytes, vid));
                }
                None => {
                    self.current_page_iter = None;
                    let num_pages = self.hf.num_pages();
                    if self.current_page_id >= num_pages {
                        return None;
                    }
                    match self.hf.read_page_from_file(self.current_page_id) {
                        Ok(page) => {
                            self.current_page_id += 1;
                            self.current_page_iter = Some(page.into_iter());
                        }
                        Err(_) => return None,
                    }
                }
            }
        }
    }
}
