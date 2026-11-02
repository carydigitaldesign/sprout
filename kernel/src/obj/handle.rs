//! A process's handle table: small integers that name objects plus the
//! rights the process holds on them.

use alloc::vec::Vec;

use sprout_abi::{Error, Handle, Rights};

use super::Object;

#[derive(Clone)]
pub struct Entry {
    pub object: Object,
    pub rights: Rights,
}

/// Slot 0 is never used, so handle 0 can mean "the kernel" in `describe`.
pub struct HandleTable {
    entries: Vec<Option<Entry>>,
}

impl HandleTable {
    pub fn new() -> HandleTable {
        HandleTable {
            entries: alloc::vec![None],
        }
    }

    /// Stores an object and returns its handle, reusing a closed slot if any.
    pub fn insert(&mut self, object: Object, rights: Rights) -> Handle {
        let entry = Some(Entry { object, rights });
        if let Some(i) = self.entries.iter().skip(1).position(Option::is_none) {
            self.entries[i + 1] = entry;
            return (i + 1) as Handle;
        }
        self.entries.push(entry);
        (self.entries.len() - 1) as Handle
    }

    pub fn get(&self, handle: Handle) -> Result<&Entry, Error> {
        if handle == 0 {
            return Err(Error::InvalidHandle);
        }
        self.entries
            .get(handle as usize)
            .and_then(Option::as_ref)
            .ok_or(Error::InvalidHandle)
    }

    pub fn remove(&mut self, handle: Handle) -> Result<Entry, Error> {
        if handle == 0 {
            return Err(Error::InvalidHandle);
        }
        self.entries
            .get_mut(handle as usize)
            .and_then(Option::take)
            .ok_or(Error::InvalidHandle)
    }

    /// Drops every handle, which also breaks a process's reference to itself.
    pub fn clear(&mut self) {
        self.entries.truncate(1);
    }

    /// Open handles.
    #[cfg_attr(not(feature = "ktest"), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }
}

crate::ktest! {
    fn handles_start_at_one_and_reuse_closed_slots() {
        use crate::mm::vmo::Vmo;
        use crate::sync::SpinLock;
        use alloc::sync::Arc;

        let vmo = || Object::Vmo(Arc::new(SpinLock::new(Vmo::new(1).expect("vmo"))));
        let mut table = HandleTable::new();
        assert_eq!(table.get(0).err(), Some(Error::InvalidHandle));
        let a = table.insert(vmo(), Rights::READ);
        let b = table.insert(vmo(), Rights::ALL);
        assert_eq!((a, b), (1, 2));
        assert_eq!(table.get(a).expect("a").rights, Rights::READ);
        assert_eq!(table.get(99).err(), Some(Error::InvalidHandle));
        table.remove(a).expect("remove a");
        assert_eq!(table.get(a).err(), Some(Error::InvalidHandle));
        assert_eq!(table.insert(vmo(), Rights::NONE), a, "closed slot is reused");
        assert_eq!(table.len(), 2);
        table.clear();
        assert_eq!(table.len(), 0);
    }
}
