use std::arch::naked_asm;

#[cfg(not(all(target_arch = "x86", target_pointer_width = "32")))]
compile_error! {"This crate only supports 32-bit x86 targets!"}

// TODO! this global is very unsafe and is currently leaking unsafety through the safe interface through data-races (Stack::dock)
//
// it should not be too hard to turn this into a thread_local, just have an utility function the asm can call to get a pointer to it
static mut STACK_START: *const u8 = std::ptr::null();

/// ### the purpose of this function:
///
/// it establishes the initial execution context and records the stack's upper boundary, known as the "dock".
///
/// this function sets up a root stack frame, calls the entry function, and ensures a clean teardown, allowing the entire system to be started and eventually return a final value.
///
/// ### what this function does:
///
/// * first, it removes its own return address and arguments (`f`, `a`) from the stack, placing them into registers for later use.
/// * it then pushes the original return address back onto the stack, followed by all standard callee-saved registers (`ebp`, `ebx`, `esi`, `edi`). this creates a predictable, restorable stack frame.
/// * next, it pushes the argument `a` for the function `f` that it is about to call.
/// * it then calculates the memory address of this argument on the stack (`esp+4`) and stores this location in the global `STACK_START` static. this address serves as the fixed "dock" point, or the highest memory address for all subsequent stack manipulations.
/// * it calls the provided function `f` with the argument `a`.
/// * once `f` returns, it pops the callee-saved registers to restore the machine state and then executes a `ret` to return to its original caller, passing along the result from `f`.
#[unsafe(naked)]
pub(crate) unsafe extern "stdcall" fn dock<A, B>(
    f: unsafe extern "stdcall" fn(*mut A) -> *mut B,
    a: *mut A,
) -> *mut B {
    naked_asm!(
        "pop eax", // pop the return address
        "pop edx", // pop the function `f`
        "pop ecx", // pop the argument `a`

        "push eax", // push the original return address back
        "push ebp",
        "push ebx",
        "push esi",
        "push edi",

        "push ecx", // push the argument `a` for `f`

        // store the current esp into STACK_START (+4 to account for the return address pushed by call)
        "lea eax, [esp-4]",
        "mov [{stack_start}], eax",

        "call edx", // call `f`

        "pop edi",
        "pop esi",
        "pop ebx",
        "pop ebp",
        "ret",
        stack_start = sym STACK_START,
    )
}

/// ### the purpose of this function:
///
/// it completely discards the current execution stack and "restarts" a new function call from the clean "dock" position.
///
/// this is a low-level way to perform a tail call that also unwinds the stack to its initial state, effectively resetting the coroutine context without creating a new one.
///
/// ### what this function does:
///
/// * it begins by removing its own return address and arguments (`f`, `a`) from the stack.
/// * it then forcefully resets the stack pointer (`esp`) to the address stored in `STACK_START`. this action instantly abandons the entire current call stack.
/// * it overwrites the argument slot on the newly reset stack (`[esp+4]`) with its own argument, `a`.
/// * finally, it performs a tail call by `jmp`ing to the provided function `f`, which will now execute on the clean stack.
///
/// ### Safety
///
/// this function is extremely unsafe because it unwinds the stack by moving the stack pointer directly. **It does not run any destructors** for objects that go out of scope. Any RAII guards (like `Box`, `Vec`, file handles, etc.) on the abandoned stack will be leaked. It must only be called when it is certain that no pending destructors need to be run.
#[unsafe(naked)]
pub(crate) unsafe extern "stdcall" fn restart<A, B>(
    f: unsafe extern "stdcall" fn(*mut A) -> *mut B,
    a: *mut A,
) -> ! {
    naked_asm!(
        "add esp, 4",               // pop the return address
        "pop edx",                  // pop the function `f`
        "pop ecx",                  // pop the argument `a`
        "mov esp, [{stack_start}]", // restore the stack to the start
        "mov [esp+4], ecx",         // change the argument to the new one
        "jmp edx",                  // jmp to `f` (tail call)
        stack_start = sym STACK_START,
    )
}

/// ### the purpose of this function:
///
/// it suspends the current execution context by capturing the active stack segment (from the current location to the "dock") and passing it to a callback function.
///
/// the callback receives a raw pointer to the stack data and its length. it is expected to save this data and then resume another context (e.g., via `resume`). if the callback returns, this function will clean up and return as if no suspension occurred.
///
/// this function returns if the callback returns of if the suspended stack was resumed
///
/// ### what this function does:
///
/// * first, it pops its own frame (return address and arguments `f`, `a`) off the stack to expose the caller's stack frame.
/// * it then pushes the return address and all callee-saved registers (`ebp`, `ebx`, `esi`, `edi`) onto the stack. this captures the complete machine state required to resume execution later.
/// * it calculates the start pointer of the stack segment to be saved (the current `esp`) and its total length (the difference between `STACK_START` and `esp`).
/// * it calls the provided callback `f`, passing it the pointer (`esi`), length (`edi`), and context argument (`a`).
/// * if the callback `f` returns, it means the suspension was aborted. the function then restores the callee-saved registers by popping them off the stack and returns normally to its caller.
#[unsafe(naked)]
pub(crate) unsafe extern "stdcall" fn suspend<A>(
    f: unsafe extern "stdcall" fn(*const u8, usize, *mut A),
    a: *mut A,
) {
    naked_asm!(
        // remove things from the stack so we can prepare it for suspend
        "pop eax", // pop the return address
        "pop edx", // pop the function
        "pop ecx", // pop the argument
        // push callee saved registers
        "push eax", // the return address
        "push ebp",
        "push ebx",
        "push esi",
        "push edi",
        // store the end of the stack to a register
        "mov esi, esp",
        // move the length of the stack
        "mov edi, [{stack_start}]", // store the start of the stack to edi
        "sub edi, esp", // then store the length (start - end)

        "push ecx", // push the 3º argument of f
        "push edi", // push the 2º argument of f
        "push esi", // push the 1º argument of f
        "call edx", // call f
        // if we reach here that means f returned and we must restore everything to as it was
        // pop callee saved registers
        "pop edi",
        "pop esi",
        "pop ebx",
        "pop ebp",
        // return (read and jump to the return address from the freshely copied stack)
        "ret",
        stack_start = sym STACK_START
    )
}

/// ### the purpose of this function:
///
/// it "lands" a previously saved stack onto the dock, overwriting the current execution context and resuming the saved one.
///
/// this is the core mechanism for switching to a suspended coroutine. because it completely replaces the current stack, this function never returns.
///
/// ### what this function does:
///
/// * it reads its arguments (`stack_data`, `stack_len`, etc.) from the stack and stores them in registers, as the stack is about to be overwritten.
/// * it calculates the new stack pointer by subtracting the `stack_len` from the `STACK_START` address.
/// * it sets the machine's stack pointer (`esp`) to this new address. the new stack is now live, though its contents are still undefined.
/// * using `rep movsb`, it performs a fast, non-stack-based memory copy, populating the new stack with the bytes from `stack_data`.
/// * it calls the post-copy callback `f`, giving the caller a chance to free the buffer that held the saved stack data.
/// * after the callback returns, it begins popping values from the newly restored stack. it first restores the callee-saved registers (`edi`, `esi`, `ebx`, `ebp`).
/// * finally, it executes a `ret`, which pops the return address from the top of the new stack and jumps to it, seamlessly resuming the suspended code.
///
/// ### Safety
///
/// this function is extremely unsafe because it overwrites the current stack by moving the stack pointer directly. **It does not run any destructors** for objects that go out of scope. Any RAII guards (like `Box`, `Vec`, file handles, etc.) on the abandoned stack will be leaked. It must only be called when it is certain that no pending destructors need to be run.
#[unsafe(naked)]
pub(crate) unsafe extern "stdcall" fn resume<A>(
    stack_data: *const u8,
    stack_len: usize,
    a: *mut A,
    f: unsafe extern "stdcall" fn(*const u8, usize, *mut A),
) -> ! {
    naked_asm!(
        // remove things from the stack so we can trash it
        "add esp, 4", // pop the return address
        "pop esi",    // pop the stack_data
        "pop ebx",    // pop the stack_len
        "pop edx",    // pop the argument
        "pop eax",    // pop the function (yes we use the stack base pointer register, we are very short on register when the stack is out of commission)

        // copy over the bytes and set esp (must not use the stack, memcpy would not work here because of that)
        "mov ecx, ebx", // the amount of bytes to copy (ecx) is the stack_len (ebx)
        // "mov esi, esi", // the start address of the source (esi) is stack_data (esi)
        "mov edi, [{stack_start}]", // the start address of the destination (edi) is stack_start...
        "sub edi, ebx", // ...minus the number of bytes of the new stack
        "mov esp, edi", // the new stack pointer is stack_start - the length of the stack
        "cld", // clear the direction flag
        "rep movsb", // copy ecx bytes from [esi] to [edi]

        "sub esi, ebx", // restore the stack_data back to its original value for f

        // call f
        "push edx", // 3º arg: a
        "push ebx", // 2º arg: stack_len
        "push esi", // 1º arg: stack_data
        "call eax",

        // pop callee saved registers (from the freshly copied stack)
        "pop edi",
        "pop esi",
        "pop ebx",
        "pop ebp",
        // return (read and jump to the return address from the freshely copied stack)
        "ret",
        stack_start = sym STACK_START
    )
}
