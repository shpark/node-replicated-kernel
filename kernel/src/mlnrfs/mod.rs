#![allow(unused)]

use crate::arch::process::UserSlice;
use crate::fs::{FileSystem, FileSystemError, MemNode, Mnode, Modes, NodeType};

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::cell::RefCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use custom_error::custom_error;
use hashbrown::HashMap;
use kpi::io::*;
use kpi::SystemCallError;
use spin::RwLock;

pub mod fd;

/// The in-memory file-system representation.
#[derive(Debug)]
pub struct MlnrFS {
    mnodes: RwLock<HashMap<Mnode, RefCell<MemNode>>>,
    files: RwLock<HashMap<String, Arc<Mnode>>>,
    root: (String, Mnode),
    nextmemnode: AtomicUsize,
}

unsafe impl Sync for MlnrFS {}

impl Default for MlnrFS {
    /// Initialize the file system from the root directory.
    fn default() -> MlnrFS {
        let rootdir = "/";
        let rootmnode = 1;

        let mut mnodes = RwLock::new(HashMap::new());
        mnodes.write().insert(
            rootmnode,
            RefCell::new(
                MemNode::new(
                    rootmnode,
                    rootdir,
                    FileModes::S_IRWXU.into(),
                    NodeType::Directory,
                )
                .unwrap(),
            ),
        );
        let mut files = RwLock::new(HashMap::new());
        files.write().insert(rootdir.to_string(), Arc::new(1));
        let root = (rootdir.to_string(), 1);

        MlnrFS {
            mnodes,
            files,
            root,
            nextmemnode: AtomicUsize::new(2),
        }
    }
}

impl MlnrFS {
    /// Get the next available memnode number.
    fn get_next_mno(&self) -> usize {
        self.nextmemnode.fetch_add(1, Ordering::Relaxed)
    }

    pub fn create(&self, pathname: &str, modes: Modes) -> Result<u64, FileSystemError> {
        // Check if the file with the same name already exists.
        match self.files.read().get(&pathname.to_string()) {
            Some(_) => return Err(FileSystemError::AlreadyPresent),
            None => {}
        }

        let mnode_num = self.get_next_mno() as u64;
        //TODO: For now all newly created mnode are for file. How to differentiate
        // between a file and a directory. Take input from the user?
        let memnode = match MemNode::new(mnode_num, pathname, modes, NodeType::File) {
            Ok(memnode) => memnode,
            Err(e) => return Err(e),
        };
        self.files
            .write()
            .insert(pathname.to_string(), Arc::new(mnode_num));
        self.mnodes.write().insert(mnode_num, RefCell::new(memnode));

        Ok(mnode_num)
    }

    pub fn write(
        &self,
        mnode_num: Mnode,
        buffer: &[u8],
        offset: usize,
    ) -> Result<usize, FileSystemError> {
        match self.mnodes.read().get(&mnode_num) {
            Some(mnode) => mnode.borrow_mut().write(buffer, offset),
            None => Err(FileSystemError::InvalidFile),
        }
    }

    pub fn read(
        &self,
        mnode_num: Mnode,
        buffer: &mut UserSlice,
        offset: usize,
    ) -> Result<usize, FileSystemError> {
        match self.mnodes.read().get(&mnode_num) {
            Some(mnode) => mnode.borrow().read(buffer, offset),
            None => Err(FileSystemError::InvalidFile),
        }
    }

    pub fn lookup(&self, pathname: &str) -> Option<Arc<Mnode>> {
        self.files
            .read()
            .get(&pathname.to_string())
            .map(|mnode| Arc::clone(mnode))
    }

    pub fn file_info(&self, mnode: Mnode) -> FileInfo {
        match self.mnodes.read().get(&mnode) {
            Some(mnode) => match mnode.borrow().get_mnode_type() {
                NodeType::Directory => FileInfo {
                    fsize: 0,
                    ftype: NodeType::Directory.into(),
                },
                NodeType::File => FileInfo {
                    fsize: mnode.borrow().get_file_size() as u64,
                    ftype: NodeType::File.into(),
                },
            },
            None => unreachable!("file_info: shouldn't reach here"),
        }
    }

    pub fn delete(&self, pathname: &str) -> Result<bool, FileSystemError> {
        match self.files.write().remove(&pathname.to_string()) {
            Some(mnode) => {
                // If the pathname is the only link to the memnode, then remove it.
                match Arc::strong_count(&mnode) {
                    1 => {
                        self.mnodes.write().remove(&mnode);
                        return Ok(true);
                    }
                    _ => {
                        self.files.write().insert(pathname.to_string(), mnode);
                        return Err(FileSystemError::PermissionError);
                    }
                }
            }
            None => return Err(FileSystemError::InvalidFile),
        };
    }

    pub fn truncate(&self, pathname: &str) -> Result<bool, FileSystemError> {
        unimplemented!("truncate");
    }

    pub fn rename(&self, oldname: &str, newname: &str) -> Result<bool, FileSystemError> {
        unimplemented!("rename");
    }
}
