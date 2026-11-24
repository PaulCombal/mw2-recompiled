// The kernel imports the recompiled code declares and this kernel does not define: a stub
// each, which logs its first call and returns 0 (unimplemented, in mod.rs), so a run
// reports which ones the game reaches. cargo xtask writes them into the recompiled tree
// (xtask/src/generate.rs's kernel_stubs) and build.rs names the file.

#[allow(unused_imports, reason = "none when every import is implemented")]
use super::unimplemented;
#[allow(unused_imports, reason = "none when every import is implemented")]
use crate::guest::kernel_import;

include!(env!("MW2_KERNEL_STUBS"));
