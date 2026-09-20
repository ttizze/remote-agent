Pinned upstream: iroh-relay 1.1.0 (Apache-2.0 OR MIT).

Bex adds numeric diagnostics at DNS, TCP, TLS, WebSocket and relay authentication
boundaries (`bex.net.stage`). The optional `bex.net.packet` target observes
WebSocket readiness, encrypted datagram fingerprints/lengths, Ping/Pong, TCP
read/write/pending/wake boundaries, and cumulative kernel counters. It captures
no addresses, identities, headers, arbitrary errors, keys or application payloads.

The socket retains its dial span to attribute later events to the same endpoint.
When packet capture is disabled, no socket sampler or forwarding waker is
created. Sampling uses a weak owner, synchronizes with socket closure, and never
duplicates the fd or extends its lifetime. Apple counters use a small C shim
compiled against the selected SDK because Darwin's packed bitfields must follow
that SDK's TCP_CONNECTION_INFO layout. Non-Apple or unavailable counters are
explicitly marked unavailable. No connection, routing, TLS or socket options
are changed. Bex's bounded in-memory recorder receives the fixed numeric events.
