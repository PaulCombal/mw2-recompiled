// Only in the executable a profile is recorded with (MW2_PGO_TRAINING,
// tools/pgo_record.sh). The runtime ends with _exit, which skips the exit
// handlers that write the profile; linked with --wrap=_exit, every _exit
// comes here and writes it first.
extern "C"
{
    int __llvm_profile_write_file(void);
    [[noreturn]] void __real__exit(int status);

    [[noreturn]] void __wrap__exit(int status)
    {
        __llvm_profile_write_file();
        __real__exit(status);
    }
}
