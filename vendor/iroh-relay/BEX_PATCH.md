Pinned upstream: iroh-relay 1.1.0 (Apache-2.0 OR MIT).

The only source changes add `bex.net.stage` tracing events at DNS, TCP, TLS,
WebSocket and relay authentication boundaries. Events contain a fixed phase and,
for address candidates, the IP family number only. No addresses, headers, keys,
errors or payloads are captured. Bex captures these events in a bounded timeline
associated with the endpoint tracing span. Connection behavior is unchanged.
