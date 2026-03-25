use crate::heap_page::HeapPage;
use crate::heapfile::HeapFile;
use crate::heapfileiter::HeapFileIterator;
use crate::page::Page;
use common::prelude::*;
use common::storage_trait::StorageTrait;
use common::testutil::gen_random_test_sm_dir;
use common::PAGE_SIZE;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::fs;

pub const STORAGE_DIR: &str = "heapstore";

pub(crate) type ContainerMap = Arc<RwLock<HashMap<ContainerId, Arc<HeapFile>>>>;
pub(crate) type ContainerPathMap = Arc<RwLock<HashMap<ContainerId, Arc<PathBuf>>>>;
const PERSIST_CONFIG_FILENAME: &str = "storage_manager";
const HEAPFILE_EXTENSION: &str = "hf";

#[derive(Serialize, Deserialize)]
pub struct StorageManager {
    pub storage_dir: PathBuf,
    is_temp: bool,
    pub(crate) cid_path_map: ContainerPathMap,
    #[serde(skip)]
    pub(crate) cid_heapfile_map: ContainerMap,
}

impl StorageManager {
    pub(crate) fn get_page(
        &self,
        container_id: ContainerId,
        page_id: PageId,
        _tid: TransactionId,
        _perm: Permissions,
        _pin: bool,
    ) -> Option<Page> {
        let map = self.cid_heapfile_map.read().unwrap();
        let hf = map.get(&container_id)?;
        hf.read_page_from_file(page_id).ok()
    }

    pub(crate) fn write_page(
        &self,
        container_id: ContainerId,
        page: &Page,
        _tid: TransactionId,
    ) -> Result<(), CrustyError> {
        let map = self.cid_heapfile_map.read().unwrap();
        let hf = map
            .get(&container_id)
            .ok_or_else(|| CrustyError::CrustyError(format!("container {} not found", container_id)))?;
        hf.write_page_to_file(page)
    }

    fn get_num_pages(&self, container_id: ContainerId) -> PageId {
        let map = self.cid_heapfile_map.read().unwrap();
        map.get(&container_id).map_or(0, |hf| hf.num_pages())
    }

    #[allow(dead_code)]
    pub(crate) fn get_hf_read_write_count(&self, container_id: ContainerId) -> (u16, u16) {
        let map = self.cid_heapfile_map.read().unwrap();
        match map.get(&container_id) {
            Some(hf) => (
                hf.read_count.load(Ordering::Relaxed),
                hf.write_count.load(Ordering::Relaxed),
            ),
            None => (0, 0),
        }
    }

    pub fn get_page_debug(&self, container_id: ContainerId, page_id: PageId) -> String {
        match self.get_page(
            container_id,
            page_id,
            TransactionId::new(),
            Permissions::ReadOnly,
            false,
        ) {
            Some(p) => format!("{:?}", p),
            None => String::new(),
        }
    }
}

impl StorageTrait for StorageManager {
    type ValIterator = HeapFileIterator;

    fn new(storage_dir: &Path) -> Self {
        let sm_file = storage_dir.join(PERSIST_CONFIG_FILENAME);
        if sm_file.exists() {
            debug!("Loading storage manager from config file {:?}", sm_file);
            let reader = fs::File::open(sm_file).expect("error opening persist config file");
            let sm: StorageManager =
                serde_json::from_reader(reader).expect("error reading from json");

            let mut hm: HashMap<ContainerId, Arc<HeapFile>> = HashMap::new();
            let mut hmfiles: HashMap<ContainerId, Arc<PathBuf>> = HashMap::new();

            let path_map: ContainerPathMap = sm.cid_path_map.clone();
            let old_files = path_map.read().unwrap();

            for (id, path) in old_files.iter() {
                let hf = HeapFile::new(path.to_path_buf(), *id)
                    .expect("Error creating/opening old HF {path}");
                hmfiles.insert(*id, Arc::new(path.to_path_buf()));
                hm.insert(*id, Arc::new(hf));
            }

            let cid_heapfile_map = Arc::new(RwLock::new(hm));
            let cid_path_map = Arc::new(RwLock::new(hmfiles));
            StorageManager {
                storage_dir: storage_dir.to_path_buf(),
                cid_heapfile_map,
                cid_path_map,
                is_temp: false,
            }
        } else {
            debug!("Making new storage_manager in directory {:?}", storage_dir);
            fs::create_dir_all(storage_dir).expect("could not create storage directory");
            StorageManager {
                storage_dir: storage_dir.to_path_buf(),
                cid_heapfile_map: Arc::new(RwLock::new(HashMap::new())),
                cid_path_map: Arc::new(RwLock::new(HashMap::new())),
                is_temp: false,
            }
        }
    }

    fn new_test_sm() -> Self {
        let storage_dir = gen_random_test_sm_dir();
        debug!("Making new temp storage_manager {:?}", storage_dir);
        fs::create_dir_all(&storage_dir).expect("could not create test storage directory");
        StorageManager {
            storage_dir,
            cid_heapfile_map: Arc::new(RwLock::new(HashMap::new())),
            cid_path_map: Arc::new(RwLock::new(HashMap::new())),
            is_temp: true,
        }
    }

    fn insert_value(
        &self,
        container_id: ContainerId,
        value: Vec<u8>,
        tid: TransactionId,
    ) -> ValueId {
        if value.len() > PAGE_SIZE {
            panic!("Cannot handle inserting a value larger than the page size");
        }
        let hf = {
            let map = self.cid_heapfile_map.read().unwrap();
            map.get(&container_id)
                .cloned()
                .expect("container not found")
        };
        let num_pages = hf.num_pages();
        for page_id in 0..num_pages {
            let mut page = hf.read_page_from_file(page_id).expect("error reading page");
            if let Some(slot_id) = page.add_value(&value) {
                hf.write_page_to_file(&page).expect("error writing page");
                return ValueId::new_slot(container_id, page_id, slot_id);
            }
        }
        let new_page_id = num_pages;
        let mut new_page = Page::new(new_page_id);
        let slot_id = new_page
            .add_value(&value)
            .expect("failed to add value to fresh page");
        hf.write_page_to_file(&new_page).expect("error writing new page");
        ValueId::new_slot(container_id, new_page_id, slot_id)
    }

    fn insert_values(
        &self,
        container_id: ContainerId,
        values: Vec<Vec<u8>>,
        tid: TransactionId,
    ) -> Vec<ValueId> {
        let mut ret = Vec::new();
        for v in values {
            ret.push(self.insert_value(container_id, v, tid));
        }
        ret
    }

    fn delete_value(&self, id: ValueId, _tid: TransactionId) -> Result<(), CrustyError> {
        let page_id = match id.page_id {
            Some(p) => p,
            None => return Ok(()),
        };
        let slot_id = match id.slot_id {
            Some(s) => s,
            None => return Ok(()),
        };
        let hf = {
            let map = self.cid_heapfile_map.read().unwrap();
            match map.get(&id.container_id).cloned() {
                Some(hf) => hf,
                None => return Ok(()),
            }
        };
        let mut page = hf.read_page_from_file(page_id)?;
        page.delete_value(slot_id);
        hf.write_page_to_file(&page)?;
        Ok(())
    }

    fn update_value(
        &self,
        value: Vec<u8>,
        id: ValueId,
        tid: TransactionId,
    ) -> Result<ValueId, CrustyError> {
        self.delete_value(id, tid)?;
        Ok(self.insert_value(id.container_id, value, tid))
    }

    fn create_container(
        &self,
        container_id: ContainerId,
        _name: Option<String>,
        _container_type: common::ids::StateType,
        _dependencies: Option<Vec<ContainerId>>,
    ) -> Result<(), CrustyError> {
        let mut path_map = self.cid_path_map.write().unwrap();
        if path_map.contains_key(&container_id) {
            return Ok(());
        }
        let file_path = self
            .storage_dir
            .join(format!("{}.{}", container_id, HEAPFILE_EXTENSION));
        let hf = HeapFile::new(file_path.clone(), container_id)?;
        path_map.insert(container_id, Arc::new(file_path));
        drop(path_map);
        self.cid_heapfile_map
            .write()
            .unwrap()
            .insert(container_id, Arc::new(hf));
        Ok(())
    }

    fn create_table(&self, container_id: ContainerId) -> Result<(), CrustyError> {
        self.create_container(container_id, None, common::ids::StateType::BaseTable, None)
    }

    fn remove_container(&self, container_id: ContainerId) -> Result<(), CrustyError> {
        let path = self.cid_path_map.write().unwrap().remove(&container_id);
        self.cid_heapfile_map.write().unwrap().remove(&container_id);
        if let Some(p) = path {
            let _ = fs::remove_file(p.as_ref());
        }
        Ok(())
    }

    fn get_iterator(
        &self,
        container_id: ContainerId,
        tid: TransactionId,
        _perm: Permissions,
    ) -> Self::ValIterator {
        let hf = {
            let map = self.cid_heapfile_map.read().unwrap();
            map.get(&container_id)
                .cloned()
                .expect("container not found")
        };
        HeapFileIterator::new(tid, hf)
    }

    fn get_iterator_from(
        &self,
        container_id: ContainerId,
        tid: TransactionId,
        _perm: Permissions,
        start: ValueId,
    ) -> Self::ValIterator {
        let hf = {
            let map = self.cid_heapfile_map.read().unwrap();
            map.get(&container_id)
                .cloned()
                .expect("container not found")
        };
        HeapFileIterator::new_from(tid, hf, start)
    }

    fn get_value(
        &self,
        id: ValueId,
        tid: TransactionId,
        perm: Permissions,
    ) -> Result<Vec<u8>, CrustyError> {
        let page_id = id
            .page_id
            .ok_or_else(|| CrustyError::CrustyError("missing page_id in ValueId".to_string()))?;
        let slot_id = id
            .slot_id
            .ok_or_else(|| CrustyError::CrustyError("missing slot_id in ValueId".to_string()))?;
        let page = self
            .get_page(id.container_id, page_id, tid, perm, false)
            .ok_or_else(|| CrustyError::CrustyError(format!("page {} not found", page_id)))?;
        page.get_value(slot_id)
            .ok_or_else(|| CrustyError::CrustyError(format!("slot {} not found", slot_id)))
    }

    fn get_storage_path(&self) -> &Path {
        &self.storage_dir
    }

    fn reset(&self) -> Result<(), CrustyError> {
        self.cid_heapfile_map.write().unwrap().clear();
        self.cid_path_map.write().unwrap().clear();
        for entry in fs::read_dir(&self.storage_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                fs::remove_file(path)?;
            } else if path.is_dir() {
                fs::remove_dir_all(path)?;
            }
        }
        Ok(())
    }

    fn clear_cache(&self) {}

    fn shutdown(&self) {
        debug!("serializing storage manager");
        let mut filename = self.storage_dir.clone();
        filename.push(PERSIST_CONFIG_FILENAME);
        serde_json::to_writer(
            fs::File::create(filename).expect("error creating file"),
            &self,
        )
        .expect("error serializing storage manager");
        self.cid_heapfile_map.write().unwrap().clear();
    }
}

impl Drop for StorageManager {
    fn drop(&mut self) {
        if self.is_temp {
            debug!("Removing storage path on drop {:?}", self.storage_dir);
            let remove_all = fs::remove_dir_all(self.storage_dir.clone());
            if let Err(e) = remove_all {
                println!("Error on removing temp dir {}", e);
            }
        }
    }
}

#[cfg(test)]
#[allow(unused_must_use)]
mod test {
    use super::*;
    use crate::storage_manager::StorageManager;
    use common::storage_trait::StorageTrait;
    use common::testutil::*;

    #[test]
    fn hs_sm_a_insert() {
        init();
        let sm = StorageManager::new_test_sm();
        let cid = 1;
        sm.create_table(cid);

        let bytes = get_random_byte_vec(40);
        let tid = TransactionId::new();

        let val1 = sm.insert_value(cid, bytes.clone(), tid);
        assert_eq!(1, sm.get_num_pages(cid));
        assert_eq!(0, val1.page_id.unwrap());
        assert_eq!(0, val1.slot_id.unwrap());

        let p1 = sm
            .get_page(cid, 0, tid, Permissions::ReadOnly, false)
            .unwrap();

        let val2 = sm.insert_value(cid, bytes, tid);
        assert_eq!(1, sm.get_num_pages(cid));
        assert_eq!(0, val2.page_id.unwrap());
        assert_eq!(1, val2.slot_id.unwrap());

        let p2 = sm
            .get_page(cid, 0, tid, Permissions::ReadOnly, false)
            .unwrap();
        assert_ne!(p1.to_bytes()[..], p2.to_bytes()[..]);
    }

    #[test]
    fn hs_sm_b_iter_small() {
        init();
        let sm = StorageManager::new_test_sm();
        let cid = 1;
        sm.create_table(cid);
        let tid = TransactionId::new();

        //Test one page
        let mut byte_vec: Vec<Vec<u8>> = vec![
            get_random_byte_vec(400),
            get_random_byte_vec(400),
            get_random_byte_vec(400),
        ];
        for val in &byte_vec {
            sm.insert_value(cid, val.clone(), tid);
        }
        let iter = sm.get_iterator(cid, tid, Permissions::ReadOnly);
        for (i, x) in iter.enumerate() {
            assert_eq!(byte_vec[i], x.0);
        }

        // Should be on two pages
        let mut byte_vec2: Vec<Vec<u8>> = vec![
            get_random_byte_vec(400),
            get_random_byte_vec(400),
            get_random_byte_vec(400),
            get_random_byte_vec(400),
        ];

        for val in &byte_vec2 {
            sm.insert_value(cid, val.clone(), tid);
        }
        byte_vec.append(&mut byte_vec2);

        let iter = sm.get_iterator(cid, tid, Permissions::ReadOnly);
        for (i, x) in iter.enumerate() {
            assert_eq!(byte_vec[i], x.0);
        }

        // Should be on 3 pages
        let mut byte_vec2: Vec<Vec<u8>> = vec![
            get_random_byte_vec(300),
            get_random_byte_vec(500),
            get_random_byte_vec(400),
        ];

        for val in &byte_vec2 {
            sm.insert_value(cid, val.clone(), tid);
        }
        byte_vec.append(&mut byte_vec2);

        let iter = sm.get_iterator(cid, tid, Permissions::ReadOnly);
        for (i, x) in iter.enumerate() {
            assert_eq!(byte_vec[i], x.0);
        }
    }

    #[test]
    #[ignore]
    fn hs_sm_b_iter_large() {
        init();
        let sm = StorageManager::new_test_sm();
        let cid = 1;

        sm.create_table(cid).unwrap();
        let tid = TransactionId::new();

        let vals = get_random_vec_of_byte_vec(1000, 40, 400);
        sm.insert_values(cid, vals, tid);
        let mut count = 0;
        for _ in sm.get_iterator(cid, tid, Permissions::ReadOnly) {
            count += 1;
        }
        assert_eq!(1000, count);
    }
}
