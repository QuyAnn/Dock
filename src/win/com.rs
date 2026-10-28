//! Balanced COM initialization for temporary worker threads and local WIC calls.
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED};

pub struct Apartment {
    initialized: bool,
    // An apartment must be torn down on the same thread that initialized it.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Apartment {
    pub fn mta() -> Self {
        Self {
            // Both S_OK and S_FALSE require a matching CoUninitialize. A thread
            // already using another apartment model must not be uninitialized here.
            initialized: unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok(),
            _thread: std::marker::PhantomData,
        }
    }

    /// Single-threaded apartment, as ShellExecute expects. A thread already in another
    /// apartment keeps it (the call fails and nothing is uninitialized).
    pub fn sta() -> Self {
        Self {
            initialized: unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok(),
            _thread: std::marker::PhantomData,
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.initialized { unsafe { CoUninitialize() }; }
    }
}
