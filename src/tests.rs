//! ```sh
//! cargo test -p stack-master --target i686-pc-windows-msvc -- --nocapture  --test-threads 1
//! ```
#![allow(static_mut_refs)]
use super::*;

#[test]
fn dock_without_suspend() {
    unsafe {
        let res = Stack::dock(|| 1234i32);
        assert_eq!(*res, 1234);
    }
}

#[test]
fn suspend_and_resume_once() {
    unsafe {
        println!("suspend_and_resume_once: A");
        Stack::dock(|| {
            println!("suspend_and_resume_once: B");
            Stack::suspend(|stack| {
                println!("suspend_and_resume_once: C");
                Stack::resume(stack)
            });
            println!("suspend_and_resume_once: D");
        });
        println!("suspend_and_resume_once: E");
    }
}
#[test]
fn suspend_and_resume_complex() {
    unsafe {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = &*Box::leak(Box::new(tx));
        let rx = &*Box::leak(Box::new(rx));
        let pump = move || {
            let recv_result = rx.try_recv();
            println!("recv_result");
            match recv_result {
                Ok(coroutine) => {
                    println!("recv coroutine:");
                    Stack::resume(coroutine)
                }
                Err(error) => {
                    println!("recv error: {error:?}");
                    Stack::restart(|| {})
                }
            }
        };
        Stack::dock(move || {
            let _ = tx.send(Stack::from_entry(move || {
                println!("started A");
                Stack::suspend(move |c| {
                    println!("suspended A: 1");
                    let _ = tx.send(c);
                    println!("sent A: 1");
                    pump()
                });
                println!("resumed A: 1");
                Stack::suspend(move |c| {
                    println!("suspended A: 2");
                    let _ = tx.send(c);
                    println!("sent A: 2");
                    pump()
                });
                println!("resumed A: 2");
                pump()
            }));
            let _ = tx.send(Stack::from_entry(move || {
                println!("started B");
                Stack::suspend(move |c| {
                    println!("suspended B: 1");
                    let _ = tx.send(c);
                    println!("sent B: 1");
                    pump()
                });
                println!("resumed B: 1");
                Stack::suspend(move |c| {
                    println!("suspended B: 2");
                    let _ = tx.send(c);
                    println!("sent B: 2");
                    pump()
                });
                println!("resumed B: 2");
                pump()
            }));
            pump()
        });
    }
}
