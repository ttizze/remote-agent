# Allow concurrent remote control

Remote Agent will not impose a single-controller or PC Host-wide single-Turn limit: multiple Paired Devices may connect and issue operations concurrently, and Codex decides which concurrency it accepts. Distinct Codex Threads may run concurrently; input sent to a Thread with an active Turn is Steering Input for that Turn, not a second independent Turn. This preserves Codex's capabilities and supports multi-device control, at the cost of requiring explicit ordering, attribution, conflict, and approval-claim rules in the Remote Agent protocol.
