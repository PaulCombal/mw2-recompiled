// The guest addresses for this build, from runtime/title.h, the only place they
// are written. build.rs makes a constant of each entry, a GuestFunction for a
// function, and a cfg of its name, so that #[cfg(T_x)] is the C++'s #ifdef T_x.
#![allow(non_upper_case_globals, dead_code, improper_ctypes)]

use crate::guest::{GuestFunction, PPCContext};

include!(concat!(env!("OUT_DIR"), "/title.rs"));

pub(crate) use hook_symbol;
