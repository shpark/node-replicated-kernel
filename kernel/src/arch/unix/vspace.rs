// Copyright © 2021 VMware, Inc. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A dummy vspace implementation for the unix platform.

use alloc::boxed::Box;
use core::fmt;
use core::pin::Pin;
use hashbrown::HashMap;

use crate::error::KError;
use crate::memory::vspace::{AddressSpace, MapAction, MappingInfo, TlbFlushHandle};
use crate::memory::Frame;

use x86::bits64::paging::*;

pub struct VSpace {
    pub mappings: HashMap<core::ops::Range<usize>, MappingInfo>,
    pub pml4: Pin<Box<PML4>>,
}

impl Default for VSpace {
    fn default() -> Self {
        VSpace::new()
    }
}

impl fmt::Debug for VSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VSpace").finish()
    }
}

impl VSpace {
    pub fn new() -> VSpace {
        VSpace {
            mappings: HashMap::new(),
            pml4: Box::pin(
                [PML4Entry::new(PAddr::from(0x0u64), PML4Flags::empty()); PAGE_SIZE_ENTRIES],
            ),
        }
    }

    pub fn map_generic(
        &mut self,
        _vbase: VAddr,
        _pregion: (PAddr, usize),
        _rights: MapAction,
        _create_mappings: bool,
        _shared: bool,
    ) -> Result<(), KError> {
        Ok(())
    }
}

impl AddressSpace for VSpace {
    fn map_frame(&mut self, base: VAddr, frame: Frame, action: MapAction) -> Result<(), KError> {
        let ma = MappingInfo::new(frame, action);
        self.mappings.insert(ma.vrange(base), ma);
        unimplemented!("map_frame");
    }

    fn map_frame_shared(&mut self, base: VAddr, frame: Frame, action: MapAction) -> Result<(), KError> {
        let ma = MappingInfo::new(frame, action);
        self.mappings.insert(ma.vrange(base), ma);
        unimplemented!("map_frame");
    }

    fn map_memory_requirements(_base: VAddr, _frames: &[Frame]) -> usize {
        unimplemented!("map_memory_requirements");
    }

    fn adjust(&mut self, _vaddr: VAddr, _rights: MapAction) -> Result<(VAddr, usize), KError> {
        unimplemented!("adjust");
    }

    fn resolve(&self, _vaddr: VAddr) -> Result<(PAddr, MapAction), KError> {
        unimplemented!("resolve");
    }

    fn unmap(&mut self, _vaddr: VAddr) -> Result<TlbFlushHandle, KError> {
        unimplemented!("unmap");
    }

    fn declassify(&mut self, _vaddr: VAddr, _nframes: usize) -> Result<(), KError> {
        unimplemented!("declassify");
    }
}

impl Drop for VSpace {
    fn drop(&mut self) {
        panic!("Drop for VSpace!");
    }
}
