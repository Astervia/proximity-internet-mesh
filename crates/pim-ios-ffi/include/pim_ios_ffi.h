/*
 * pim_ios_ffi.h — C ABI for the Proximity Internet Mesh iOS bridge.
 *
 * Resolves milestone 1 of issue #70. The surface is intentionally
 * minimal in Plan 1:
 *
 *   - pim_ffi_version()      returns a library-owned version string.
 *   - pim_ffi_start()        allocates an opaque handle and validates
 *                            the config JSON shape.
 *   - pim_ffi_stop()         releases a handle.
 *   - pim_ffi_free_string()  frees an error string from pim_ffi_start.
 *
 * Plan 2 extends the surface with read/write packet callbacks that the
 * NEPacketTunnelProvider extension registers so the Rust core can drive
 * NEPacketTunnelFlow.
 */
#ifndef PIM_IOS_FFI_H
#define PIM_IOS_FFI_H

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle; contents are private to the Rust side. */
typedef struct PimHandle PimHandle;

/*
 * Return a pointer to a library-owned NUL-terminated UTF-8 string.
 * The caller must not free the returned pointer. The lifetime is the
 * lifetime of the library.
 */
const char *pim_ffi_version(void);

/*
 * Start the PIM runtime with the given JSON config.
 *
 * Success:   returns a non-NULL PimHandle*. The caller owns it and must
 *            release it by calling pim_ffi_stop() exactly once.
 * Failure:   returns NULL. If `err_out` is non-NULL, *err_out is set to
 *            a library-owned NUL-terminated UTF-8 string describing the
 *            error. The caller must release that string by calling
 *            pim_ffi_free_string().
 *
 * Plan 1 validates the JSON shape only — it does not yet start the
 * daemon.
 */
PimHandle *pim_ffi_start(const char *config_json, char **err_out);

/*
 * Stop and free a handle previously returned by pim_ffi_start.
 *   - Passing NULL is a no-op.
 *   - Passing the same non-NULL handle twice is undefined behavior.
 */
void pim_ffi_stop(PimHandle *handle);

/*
 * Release an error string previously produced by pim_ffi_start.
 * Passing NULL is a no-op.
 */
void pim_ffi_free_string(char *s);

#ifdef __cplusplus
}
#endif

#endif /* PIM_IOS_FFI_H */
