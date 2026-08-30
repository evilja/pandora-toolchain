// Pandora Mini: a Discord-less node that leases whole jobs from a coordinating `pndc`, fetches
// their sources itself, runs them through the ordinary worker runtime, and reports back.
//
// `spec` is the wire contract both sides compile. `board` is the coordinator's cluster state, the
// bridge between the axum link routes and `pn_worker`'s loop. `client` is the node half. A
// coordinator with no registered nodes behaves exactly as it did before any of this existed.
pub mod board;
pub mod client;
pub mod coordinator;
pub mod spec;
