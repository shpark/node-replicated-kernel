#![allow(unused)]

use crate::error::KError;
use crate::fs::{
    Buffer, FileDescriptor, FileSystem, FileSystemError, Filename, Flags, Len, Modes, Offset, FD,
};
use crate::mlnrfs::{fd::FileDesc, MlnrFS};
use crate::prelude::*;
use crate::process::{userptr_to_str, Eid, Executor, KernSlice, Pid, Process, ProcessError};

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use hashbrown::HashMap;
use kpi::{io::*, FileOperation};
use mlnr::{Dispatch, LogMapper, ReplicaToken};
use spin::RwLock;

pub struct MlnrKernelNode {
    counters: Vec<CachePadded<AtomicUsize>>,
    process_map: RwLock<HashMap<Pid, FileDesc>>,
    fs: MlnrFS,
}

impl Default for MlnrKernelNode {
    fn default() -> Self {
        let max_cores = 192;
        let mut counters = Vec::with_capacity(max_cores);
        for _i in 0..max_cores {
            counters.push(Default::default());
        }
        MlnrKernelNode {
            counters,
            process_map: RwLock::new(HashMap::with_capacity(256)),
            fs: MlnrFS::default(),
        }
    }
}

#[derive(Hash, Clone, Debug, PartialEq)]
pub enum Modify {
    Increment(usize),
    ProcessAdd(Pid),
    ProcessRemove(Pid),
    FileOpen(Pid, String, Flags, Modes),
    FileWrite(Pid, FD, Arc<[u8]>, Len, Offset),
    FileClose(Pid, FD),
    FileDelete(Pid, String),
    FileRename(Pid, String, String),
}

impl LogMapper for Modify {
    fn hash(&self) -> usize {
        0
    }
}

impl Default for Modify {
    fn default() -> Self {
        Modify::Increment(0)
    }
}

#[derive(Hash, Clone, Debug, PartialEq)]
pub enum Access {
    Get,
    FileRead(Pid, FD, Buffer, Len, Offset),
    FileInfo(Pid, Filename, u64),
}

impl LogMapper for Access {
    fn hash(&self) -> usize {
        0
    }
}

#[derive(Clone, Debug)]
pub enum MlnrNodeResult {
    Incremented(u64),
    ProcessAdded(Pid),
    FileOpened(FD),
    FileAccessed(Len),
}

impl MlnrKernelNode {
    pub fn mlnr_direct_bench() -> Result<(u64, u64), KError> {
        let kcb = super::kcb::get_kcb();
        kcb.arch
            .mlnr_replica
            .as_ref()
            .map_or(Err(KError::ReplicaNotSet), |(replica, token)| {
                let response = replica.execute_mut(Modify::Increment(token.id()), *token);
                match &response {
                    Ok(MlnrNodeResult::Incremented(val)) => Ok((*val, 0)),
                    Ok(_) => unreachable!("Got unexpected response"),
                    Err(e) => Err(e.clone()),
                }
            })
    }

    pub fn add_process(pid: u64) -> Result<(u64, u64), KError> {
        let kcb = super::kcb::get_kcb();
        kcb.arch
            .mlnr_replica
            .as_ref()
            .map_or(Err(KError::ReplicaNotSet), |(replica, token)| {
                let response = replica.execute_mut(Modify::ProcessAdd(pid), *token);
                match &response {
                    Ok(MlnrNodeResult::ProcessAdded(pid)) => Ok((*pid, 0)),
                    Ok(_) => unreachable!("Got unexpected response"),
                    Err(e) => Err(e.clone()),
                }
            })
    }

    pub fn map_fd(pid: Pid, pathname: u64, flags: u64, modes: u64) -> Result<(FD, u64), KError> {
        let kcb = super::kcb::get_kcb();
        kcb.arch
            .mlnr_replica
            .as_ref()
            .map_or(Err(KError::ReplicaNotSet), |(replica, token)| {
                let filename;
                match userptr_to_str(pathname) {
                    Ok(user_str) => filename = user_str,
                    Err(e) => return Err(e.clone()),
                }

                let response =
                    replica.execute_mut(Modify::FileOpen(pid, filename, flags, modes), *token);

                match &response {
                    Ok(MlnrNodeResult::FileOpened(fd)) => Ok((*fd, 0)),
                    Ok(_) => unreachable!("Got unexpected response"),
                    Err(r) => Err(r.clone()),
                }
            })
    }

    pub fn file_io(
        op: FileOperation,
        pid: Pid,
        fd: u64,
        buffer: u64,
        len: u64,
        offset: i64,
    ) -> Result<(Len, u64), KError> {
        let kcb = super::kcb::get_kcb();
        kcb.arch
            .mlnr_replica
            .as_ref()
            .map_or(Err(KError::ReplicaNotSet), |(replica, token)| match op {
                FileOperation::Write | FileOperation::WriteAt => {
                    let kernslice = KernSlice::new(buffer, len as usize);

                    let response = replica.execute_mut(
                        Modify::FileWrite(pid, fd, kernslice.buffer.clone(), len, offset),
                        *token,
                    );

                    match &response {
                        Ok(MlnrNodeResult::FileAccessed(len)) => Ok((*len, 0)),
                        Ok(_) => unreachable!("Got unexpected response"),
                        Err(r) => Err(r.clone()),
                    }
                }
                _ => unreachable!(),
            })
    }
}

impl Dispatch for MlnrKernelNode {
    type ReadOperation = Access;
    type WriteOperation = Modify;
    type Response = Result<MlnrNodeResult, KError>;

    fn dispatch(&self, _op: Self::ReadOperation) -> Self::Response {
        Ok(MlnrNodeResult::Incremented(
            self.counters[0].load(Ordering::Relaxed) as u64,
        ))
    }

    fn dispatch_mut(&self, op: Self::WriteOperation) -> Self::Response {
        match op {
            Modify::Increment(tid) => Ok(MlnrNodeResult::Incremented(
                self.counters[tid].fetch_add(1, Ordering::Relaxed) as u64,
            )),

            Modify::ProcessAdd(pid) => {
                match self.process_map.write().insert(pid, FileDesc::default()) {
                    Some(_) => Err(KError::ProcessError {
                        source: crate::process::ProcessError::NotEnoughMemory,
                    }),
                    None => Ok(MlnrNodeResult::ProcessAdded(pid)),
                }
            }

            Modify::ProcessRemove(pid) => unimplemented!("Process Remove"),

            Modify::FileOpen(pid, filename, flags, modes) => {
                let flags = FileFlags::from(flags);
                let mnode = self.fs.lookup(&filename);
                if mnode.is_none() && !flags.is_create() {
                    return Err(KError::FileSystem {
                        source: FileSystemError::PermissionError,
                    });
                }
                let mut process_map = self.process_map.write();
                let fd = process_map.get_mut(&pid).unwrap().allocate_fd();

                match fd {
                    None => Err(KError::NotSupported),
                    Some(mut fd) => {
                        let mnode_num;
                        if mnode.is_none() {
                            match self.fs.create(&filename, modes) {
                                Ok(m_num) => mnode_num = m_num,
                                Err(e) => {
                                    let fdesc = fd.0 as usize;
                                    process_map.get_mut(&pid).unwrap().deallocate_fd(fdesc);
                                    return Err(KError::FileSystem { source: e });
                                }
                            }
                        } else {
                            // File exists and FileOpen is called with O_TRUNC flag.
                            if flags.is_truncate() {
                                self.fs.truncate(&filename);
                            }
                            mnode_num = *mnode.unwrap();
                        }
                        fd.1.update_fd(mnode_num, flags);
                        Ok(MlnrNodeResult::FileOpened(fd.0))
                    }
                }
            }

            Modify::FileWrite(pid, fd, kernslice, len, offset) => {
                let mut process_lookup = self.process_map.read();
                let fd = process_lookup.get(&pid).unwrap().get_fd(fd as usize);
                let mnode_num = fd.get_mnode();
                let flags = fd.get_flags();

                // Check if the file has write-only or read-write permissions before reading it.
                if !flags.is_write() {
                    return Err(KError::FileSystem {
                        source: FileSystemError::PermissionError,
                    });
                }

                let mut curr_offset: usize = offset as usize;
                if offset == -1 {
                    if flags.is_append() {
                        // If offset value is not provided and file is opened with O_APPEND flag.
                        let finfo = self.fs.file_info(mnode_num);
                        curr_offset = finfo.fsize as usize;
                    } else {
                        // If offset value is not provided and file is doesn't have O_APPEND flag.
                        curr_offset = fd.get_offset();
                    }
                }

                match self.fs.write(mnode_num, &kernslice.clone(), curr_offset) {
                    Ok(len) => {
                        if offset == -1 {
                            // Update offset when FileWrite doesn't give an explicit offset value.
                            fd.update_offset(curr_offset + len);
                        }
                        Ok(MlnrNodeResult::FileAccessed(len as u64))
                    }
                    Err(e) => Err(KError::FileSystem { source: e }),
                }
            }

            Modify::FileClose(pid, fd) => unimplemented!("File Close"),

            Modify::FileDelete(pid, filename) => unimplemented!("File Delete"),

            Modify::FileRename(pid, oldname, newname) => unimplemented!("File Rename"),
        }
    }
}
