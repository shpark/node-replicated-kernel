use alloc::vec::Vec;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::ptr;
use core::mem;

use fringe::generator::Generator;
use rawtime::Instant;

use crate::stack::LineupStack;
use crate::tls2::{self, ThreadControlBlock};
use crate::upcalls::Upcalls;
use crate::CoreId;

/// Type alias for our generic generator.
pub(crate) type Runnable<'a> = Generator<'a, YieldResume, YieldRequest, LineupStack>;

/// The id of a thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ThreadId(pub usize);

impl Hash for ThreadId {
    /// For hashing we only rely on the ID as the affinity can change.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ThreadId {{ id={} }}", self.0)
    }
}

pub(crate) struct Thread {
    pub(crate) id: ThreadId,
    pub(crate) affinity: CoreId,
    pub(crate) return_with: Option<YieldResume>,

    /// Storage to remember the pointer to the TCB
    ///
    /// If a thread runs the first time this is null since a thread creates
    /// it's own TCB before running. After the first yield this will
    /// be used to memorize it for future resumes.
    ///
    /// TODO(correctness): It's not really static (it's on the thread's stack),
    /// but keeps it easier for now.
    pub(crate) state: *mut ThreadControlBlock<'static>,
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Thread#{}", self.id.0)
    }
}

impl PartialEq for Thread {
    fn eq(&self, other: &Thread) -> bool {
        self.id.0 == other.id.0
    }
}

impl Eq for Thread {}

impl Hash for Thread {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Thread {
    pub(crate) unsafe fn new<'a, F>(
        tid: ThreadId,
        affinity: CoreId,
        stack: LineupStack,
        f: F,
        arg: *mut u8,
        upcalls: Upcalls,
    ) -> (
        Thread,
        Generator<'a, YieldResume, YieldRequest, LineupStack>,
    )
    where
        F: 'static + FnOnce(*mut u8) + Send,
    {
        let thread = Thread {
            id: tid,
            affinity,
            return_with: None,
            state: ptr::null_mut(),
        };

        let generator = Generator::unsafe_new(stack, move |yielder, _| {
            let mut ts = tls2::ThreadControlBlock {
                tid,
                yielder,
                upcalls,
                current_core: affinity,
                rump_lwp: ptr::null_mut(),
                rumprun_lwp: ptr::null_mut(),
            };

            let (initial_tdata, tls_layout) = crate::tls2::arch::calculate_tls_size2();
            log::info!("initial_tdata.len() = {} tls_layout = {:?}", initial_tdata.len(), tls_layout);
            // Set up a TLS block (variant 2: [tdata, tbss, TCB], and start of TCB goes in fs)
            let tls_base: *mut u8 = alloc::alloc::alloc_zeroed(tls_layout);
            // TODO(correctness): So this doesn't really respect alignment of ThreadControlBlock :(
            let tcb = tls_base.offset((tls_layout.size() - mem::size_of::<ThreadControlBlock>()) as isize);
            *(tcb as *mut ThreadControlBlock) = ts;
            tls_base.copy_from_nonoverlapping(initial_tdata.as_ptr(), initial_tdata.len());

            // Install TCB/TLS
            tls2::arch::set_tcb(tcb as *mut ThreadControlBlock);

            let r = f(arg);

            // Reset TCB/TLS once thread completes
            tls2::arch::set_tcb(ptr::null_mut() as *mut ThreadControlBlock);
            alloc::alloc::dealloc(tls_base, tls_layout);
            r
        });

        (thread, generator)
    }
}

/// Requests that go from the thread-context to the scheduler.
#[derive(Debug, PartialEq)]
pub(crate) enum YieldRequest {
    /// Just yield for now?
    None,
    /// Block thread until we reach Instant.
    Timeout(Instant),
    /// Tell scheduler to make ThreadId runnable.
    Runnable(ThreadId),
    /// Tell scheduler to make ThreadId unrunnable.
    Unrunnable(ThreadId),
    /// Make everything in the given list runnable.
    RunnableList(Vec<ThreadId>),
    /// Spawn a new thread that runs the provided function and argument.
    Spawn(
        Option<unsafe extern "C" fn(arg1: *mut u8) -> *mut u8>,
        *mut u8,
        CoreId,
    ),
    /// Spawn a new thread that runs function/argument on the provided stack.
    SpawnWithStack(
        LineupStack,
        Option<unsafe extern "C" fn(arg1: *mut u8) -> *mut u8>,
        *mut u8,
        CoreId,
    ),
}

/// Corresponding response to a thread after we yielded back to
/// the scheduler with a request (see `YieldRequest`)
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum YieldResume {
    /// The request was completed (we immediately resumed without a context switch).
    Completed,
    /// The thread was done (and is resumed now after a context switch).
    Interrupted,
    /// A child thread was spawned with the given ThreadId.
    Spawned(ThreadId),
    /// Thread has completed (and has been removed from the scheduler state)
    DoNotResume,
}
