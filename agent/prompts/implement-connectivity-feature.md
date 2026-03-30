# Implement Connectivity Feature

Implement a new peer connectivity mechanism for this repository.

Requirements:

- Preserve the current separation between peer discovery or link setup and the direct peer transport.
- Reuse the daemon's existing connection initiation and handshake path whenever possible.
- Add config in `pim-core`.
- Keep the feature opt-in and safe by default.
- Add tests for config parsing and service construction at minimum.
- Update architecture docs for any changed behavior.

Repository-specific guidance:

- The daemon currently always starts `TcpTransport`.
- UDP discovery and Wi-Fi Direct both feed the same connection path.
- Favor adding a new discovery or link-establishment module over rewriting transport.

Expected output:

1. Short implementation plan.
2. Concrete code changes.
3. Verification results.
4. Risks, limitations, and follow-up work.
