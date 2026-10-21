//! Activity Monitor's logic: no syscalls, everything takes text in and gives values out.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod audit;
pub mod fmt;
pub mod proc;
pub mod system;
