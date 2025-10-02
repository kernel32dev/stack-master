# stack-master

A minimal, unsafe library for low-level stackful coroutines on 32-bit x86 systems

This crate provides the fundamental building blocks for context switching

Implemented using stack copying, suspend is implemented by taking bytes from the stack and resume is implemented by placing them back