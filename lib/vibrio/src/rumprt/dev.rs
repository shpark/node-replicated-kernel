use super::{c_int, c_uint, c_ulong, c_void};

/*use crate::arch::memory::{kernel_vaddr_to_paddr, PAddr, VAddr};
use crate::arch::vspace::{MapAction, VSpace};
use crate::kcb::{get_kcb, Kcb};
use crate::memory::PhysicalAllocator;*/

use alloc::boxed::Box;
use core::alloc::Layout;
use core::cell::RefMut;
use core::fmt;
use core::ptr;

use log::trace;
use x86::current::paging::{PAddr, VAddr};
use x86::io;

use log::{error, info, warn};

static PCI_CONF_ADDR: u16 = 0xcf8;
static PCI_CONF_DATA: u16 = 0xcfc;

#[inline]
fn pci_bus_address(bus: u32, dev: u32, fun: u32, reg: i32) -> u32 {
    assert!(reg <= 0xfc);

    (1 << 31) | (bus << 16) | (dev << 11) | (fun << 8) | (reg as u32 & 0xfc)
}

#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_iospace_init() -> c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_confread(
    bus: c_uint,
    dev: c_uint,
    fun: c_uint,
    reg: c_int,
    value: *mut c_uint,
) -> c_int {
    let addr = pci_bus_address(bus, dev, fun, reg);

    io::outl(PCI_CONF_ADDR, addr);
    *value = io::inl(PCI_CONF_DATA);
    trace!(
        "rumpcomp_pci_confread ({:#x} {:#x} {:#x}) reg({}) val = {:#x}",
        bus,
        dev,
        fun,
        reg,
        *value
    );

    0
}

#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_confwrite(
    bus: c_uint,
    dev: c_uint,
    fun: c_uint,
    reg: c_int,
    value: c_uint,
) -> c_int {
    trace!(
        "rumpcomp_pci_confwrite ({:#x} {:#x} {:#x}) reg({:#x}) = value({:#x})",
        bus,
        dev,
        fun,
        reg,
        value
    );

    let addr = pci_bus_address(bus, dev, fun, reg);
    io::outl(PCI_CONF_ADDR, addr);
    io::outl(PCI_CONF_DATA, value);
    0
}

#[derive(Debug, Copy, Clone)]
struct RumpIRQ {
    tuple: (c_uint, c_uint, c_uint),
    vector: c_int,
    cookie: c_uint,
    handler: Option<unsafe extern "C" fn(arg: *mut c_void) -> c_int>,
    arg: *mut c_void,
}

static mut IRQS: [RumpIRQ; 32] = [RumpIRQ {
    tuple: (0, 0, 0),
    vector: 0,
    cookie: 0,
    handler: None,
    arg: ptr::null_mut(),
}; 32];

//int rumpcomp_pci_irq_map(unsigned bus, unsigned device, unsigned fun, int intrline, unsigned cookie)
#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_irq_map(
    bus: c_uint,
    dev: c_uint,
    fun: c_uint,
    vector: c_int,
    cookie: c_uint,
) -> c_int {
    trace!(
        "rumpcomp_pci_irq_map for ({:#x} {:#x} {:#x}) IRQ={:#x} {:#x}",
        bus,
        dev,
        fun,
        vector,
        cookie
    );
    IRQS[0].tuple = (bus, dev, fun);
    IRQS[0].vector = vector;
    IRQS[0].cookie = cookie;

    crate::syscalls::irqalloc(vector as u64, 0).ok();

    0
}

#[allow(unused)]
pub unsafe extern "C" fn irq_handler(_arg1: *mut u8) -> *mut u8 {
    let s = lineup::tls::Environment::scheduler();
    let upcalls = s.rump_upcalls as *const super::RumpHyperUpcalls;

    (*upcalls).hyp_schedule.expect("rump_upcalls set")();
    (*upcalls).hyp_lwproc_newlwp.expect("rump_upcalls set")(0);
    (*upcalls).hyp_unschedule.expect("rump_upcalls set")();
    info!("irq_handler");

    let mut nlock: i32 = 1;
    loop {
        //x86::irq::disable();

        super::rumpkern_sched(&nlock, None);
        let _r = (IRQS[0].handler.unwrap())(IRQS[0].arg as *mut u64);
        //assert_eq!(r, 0, "IRQ handler should return 0?");
        super::rumpkern_unsched(&mut nlock, None);

        //crate::arch::irq::acknowledge();
        //x86::irq::enable();

        let thread = lineup::tls::Environment::thread();
        thread.block(); // Wake up on next IRQ
    }
}

#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_irq_establish(
    cookie: c_uint,
    handler: Option<unsafe extern "C" fn(arg: *mut c_void) -> c_int>,
    arg: *mut c_void,
) -> *mut c_void {
    trace!("rumpcomp_pci_irq_establish {:#x} {:p}", cookie, arg);
    IRQS[0].handler = handler;
    IRQS[0].arg = arg;
    warn!("register for IRQ {}", IRQS[0].vector as usize + 31);

    &mut IRQS[0] as *mut _ as *mut c_void
}

use core::hash::{Hash, Hasher};
use hashmap_core::map::HashMap;
use spin::Mutex;

lazy_static! {
    static ref VADDR_TO_PADDR: Mutex<HashMap<u64, u64>> = {
        let mut m = HashMap::with_capacity(128);
        Mutex::new(m)
    };
}

#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_map(addr: c_ulong, len: c_ulong) -> *mut c_void {
    trace!("rumpcomp_pci_map {:#x} {:#x}", addr, len);

    let start = PAddr::from(addr);
    let end = PAddr::from(addr) + len;

    let r = crate::syscalls::vspace(
        crate::syscalls::VSpaceOperation::MapDevice,
        start.as_u64(),
        end.as_u64(),
    );

    match r {
        Ok((vaddr, paddr)) => vaddr.as_u64() as *mut c_void,
        Err(e) => ptr::null_mut(),
    }
}

// Return PAddr for VAddr
#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_virt_to_mach(vaddr: *mut c_void) -> c_ulong {
    let vaddr = VAddr::from(vaddr as u64);

    let (_, paddr) = crate::syscalls::vspace(
        crate::syscalls::VSpaceOperation::Identify,
        vaddr.align_down_to_base_page().into(),
        0x0,
    )
    .unwrap();
    let paddr = paddr + vaddr.base_page_offset();

    trace!(
        "rumpcomp_pci_virt_to_mach va:{:#x} -> pa:{:#x}",
        vaddr,
        paddr
    );

    paddr.as_u64()
}

#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_dmalloc(
    size: usize,
    alignment: usize,
    pptr: *mut c_ulong,
    vptr: *mut c_ulong,
) -> c_int {
    let layout = Layout::from_size_align_unchecked(size, alignment);

    let mut p = crate::mem::PAGER.lock();
    let r = (*p).allocate_new(layout);
    match r {
        Ok((vaddr, paddr)) => {
            *vptr = vaddr.as_u64();
            *pptr = paddr.as_u64();
            info!(
                "rumpcomp_pci_dmalloc {:#x} {:#x} at va:{:#x} pa:{:#x}",
                size,
                alignment,
                vaddr.as_u64(),
                paddr.as_u64()
            );

            0
        }
        Err(e) => 1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_dmafree(addr: c_ulong, size: usize) {
    error!("rumpcomp_pci_dmafree {:#x} {:#x}", addr, size);
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct rumpcomp_pci_dmaseg {
    pub ds_pa: c_ulong,
    pub ds_len: c_ulong,
    pub ds_vacookie: c_ulong,
}

impl fmt::Debug for rumpcomp_pci_dmaseg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "rumpcomp_pci_dmaseg {{ ds_pa: {:#x}, ds_len: {}, ds_vacookie: {:#x} }}",
            self.ds_pa, self.ds_len, self.ds_vacookie
        )
    }
}

#[no_mangle]
pub unsafe extern "C" fn rumpcomp_pci_dmamem_map(
    dss: *mut rumpcomp_pci_dmaseg,
    nseg: usize,
    totlen: usize,
    vap: *mut *mut c_void,
) -> c_int {
    info!(
        "rumpcomp_pci_dmamem_map {:#x} {:#x} {:?}",
        nseg,
        totlen,
        &mut (*dss)
    );

    if nseg <= 1 {
        *vap = ((*dss).ds_vacookie) as *mut c_void;
        //trace!("rumpcomp_pci_dmamem_map vap={:p}", *vap);
        0
    } else {
        panic!("nseg > 1");
        1
    }
}