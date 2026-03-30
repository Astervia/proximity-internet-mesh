# Review Connectivity Change

Review a change that touches peer discovery, transport, connection setup, routing, or gateway behavior.

Priorities:

1. Behavioral regressions in peer connection setup
2. Breaks in handshake or session establishment
3. Mis-layering between discovery and transport
4. Reconnect or deduplication bugs
5. Missing config validation or tests

Focus questions:

- Does the change preserve the existing `initiate_peer_connection()` flow where appropriate?
- Does the new mechanism duplicate logic that should remain shared?
- Are config defaults safe and backward compatible?
- Are failure, timeout, and cleanup paths handled?
- Are docs aligned with the actual code?

Output format:

- findings first, ordered by severity
- open questions or assumptions
- short summary only after findings
