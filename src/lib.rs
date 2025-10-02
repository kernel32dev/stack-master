mod asm;
#[cfg(test)]
mod tests;

/// The `Stack` type represents a saved stack which can be resumed later.
///
/// See it's static methods for more
pub struct Stack(StackImpl);

enum StackImpl {
    Boxed(Box<[u8]>),
    Empty {
        f: unsafe extern "stdcall" fn(*mut ()) -> *mut (),
        a: *mut (),
        drop_a: unsafe fn(*mut ()),
    },
}

impl Stack {
    /// the dock function enables the use of [`Stack::suspend`] inside of the entry function
    ///
    /// ## SAFETY
    /// it is undefined behaviour to call this function inside a call to [`Stack::dock`]
    pub unsafe fn dock<T>(entry: impl FnOnce() -> T + 'static) -> Box<T> {
        use std::mem::ManuallyDrop;
        unsafe extern "stdcall" fn fn_entry<F, T>(entry: *mut ManuallyDrop<F>) -> *mut T
        where
            F: FnOnce() -> T,
        {
            unsafe { Box::into_raw(Box::new(ManuallyDrop::into_inner(std::ptr::read(entry))())) }
        }

        let mut entry = ManuallyDrop::new(entry);

        unsafe { Box::from_raw(asm::dock(fn_entry, &mut entry as *mut _)) }
    }

    /// creates a new stack that when resumed will run the specified entry function
    ///
    /// if the passed function returns, when this stack is being executed after being resumed, [`Stack::dock`] will quit and return that value
    ///
    /// ## SAFETY
    /// it is undefined behaviour for entry to unwind
    pub unsafe fn from_entry<F, T>(entry: F) -> Stack
    where
        F: FnOnce() -> T + 'static,
    {
        let f = unsafe {
            std::mem::transmute::<
                unsafe extern "stdcall" fn(*mut F) -> *mut T,
                unsafe extern "stdcall" fn(*mut ()) -> *mut (),
            >(boxed_entry::<F, T>)
        };
        let a = Box::into_raw(Box::new(entry)) as *mut ();

        Stack(StackImpl::Empty {
            f,
            a,
            drop_a: boxed_drop::<F>,
        })
    }

    /// discards the current stack without unwinding or running destructors, and replaces it with a call to entry
    ///
    /// can be thought of as a short of creating a stack with [`Stack::from_entry`] and immediatly calling [`Stack::resume`] on it
    ///
    /// if the passed function returns, [`Stack::dock`] will quit and return that value
    ///
    /// ## SAFETY
    /// it is undefined behaviour to:
    /// - call this function inside a call to [`Stack::dock`]
    /// - for entry to unwind
    pub unsafe fn restart<T>(entry: impl FnOnce() -> T + 'static) -> ! {
        unsafe { asm::restart(boxed_entry, Box::into_raw(Box::new(entry))) }
    }

    /// takes the current stack into an stores it into an instance of Stack, which can later be resumed
    ///
    /// the callback serves as an opportunity to call [`Stack::restart`] to start a new
    ///
    /// ## SAFETY
    /// it is undefined behaviour to:
    /// - call this function outside a call to [`Stack::dock`]
    /// - for f to unwind
    pub unsafe fn suspend<F>(f: F)
    where
        F: FnOnce(Stack) -> std::convert::Infallible + 'static,
    {
        // The trampoline matches the callback signature expected by `asm::suspend`.
        // It is nested and generic over F so we can move the actual closure in-place.
        unsafe extern "stdcall" fn suspend_trampoline<F>(
            stack_data: *const u8,
            stack_len: usize,
            fn_ptr: *mut F,
        ) where
            F: FnOnce(Stack) -> std::convert::Infallible,
        {
            println!("stack_data = {stack_data:?}");
            println!("stack_len = {stack_len:?}");
            println!("fn_ptr = {fn_ptr:?}");
            // Safety: we're called from the special assembly `suspend` which
            // provides a valid `stack_data` and `stack_len`. We allocate on the heap
            // and copy the bytes out of the current stack region into the heap buffer.
            let coroutine = unsafe { Stack::from_parts_copied(stack_data, stack_len) };

            let f = unsafe { Box::from_raw(fn_ptr) };

            // call the user's closure; it returns `Infallible` (never), so we never return.
            let _ = f(coroutine);
        }

        unsafe {
            let f = Box::into_raw(Box::new(f));
            // call the assembly helper which will call our trampoline with (stack_data, stack_len, &mut entry)
            asm::suspend(suspend_trampoline::<F>, f);
        }
    }

    /// discards the current stack without unwinding or running destructors, and replaces it with the specified stack, consuming it
    ///
    /// it is undefined behaviour to:
    ///
    /// ## SAFETY
    /// - call this function outside a call to [`Stack::dock`]
    /// - call this function with a stack suspended from a different call to [`Stack::dock`]
    /// - call this function with a stack that was created with a output type that is different from the output type of [`Stack::dock`]
    pub unsafe fn resume(mut stack: Stack) -> ! {
        unsafe extern "stdcall" fn land_drop_coroutine_trampoline(
            stack_data: *const u8,
            stack_len: usize,
            _: *mut (),
        ) {
            unsafe {
                drop(Stack::from_parts_owned(stack_data as *mut u8, stack_len));
            }
        }

        match stack.0 {
            StackImpl::Boxed(ref mut bytes) => {
                let raw = Box::into_raw(std::mem::take(bytes));
                let stack_data = raw as *mut u8;
                let stack_len = raw.len();
                unsafe {
                    // Call the underlying assembly function to land the new stack.
                    asm::resume(
                        stack_data,
                        stack_len,
                        // We don't need to pass any context to our no-op callback.
                        std::ptr::null_mut(),
                        land_drop_coroutine_trampoline,
                    );
                }
            }
            StackImpl::Empty {
                f,
                ref mut a,
                drop_a: _,
            } => unsafe { asm::restart(f, std::mem::take(a)) },
        }
    }

    pub(crate) unsafe fn from_parts_owned(stack_data: *mut u8, stack_len: usize) -> Self {
        unsafe {
            Stack(StackImpl::Boxed(Box::from_raw(
                std::ptr::slice_from_raw_parts_mut(stack_data, stack_len),
            )))
        }
    }
    pub(crate) unsafe fn from_parts_copied(stack_data: *const u8, stack_len: usize) -> Self {
        unsafe {
            Stack(StackImpl::Boxed(Box::from(std::slice::from_raw_parts(
                stack_data, stack_len,
            ))))
        }
    }
}

impl Drop for StackImpl {
    fn drop(&mut self) {
        match *self {
            StackImpl::Boxed(_) => {}
            StackImpl::Empty { f: _, a, drop_a } => unsafe {
                if !a.is_null() {
                    drop_a(a);
                }
            },
        }
    }
}

unsafe extern "stdcall" fn boxed_entry<F, T>(entry: *mut F) -> *mut T
where
    F: FnOnce() -> T,
{
    unsafe { Box::into_raw(Box::new(Box::from_raw(entry)())) }
}

unsafe fn boxed_drop<T>(entry: *mut ()) {
    unsafe { drop(Box::from_raw(entry as *mut T)) }
}
