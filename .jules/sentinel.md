## 2024-05-15 - Validate Interface Names Before Passing to System Commands
**Vulnerability:** Shell arguments like network interfaces (e.g. `br-nap`, `eth0`) could potentially be vulnerable to argument injection or invalid characters if passed directly to system commands like `ip` or `ifconfig`.
**Learning:** `Command::new` natively protects against shell injection by passing arguments directly to the process, but validating inputs explicitly prevents argument injection and ensures unexpected control characters aren't processed by system binaries.
**Prevention:** Implement an explicit validator like `is_safe_interface_name` to strictly allow alphanumeric, dashes, underscores, and dots, with a max length constraint (e.g. 15 characters).
