//! Browser access through Cloudflare, run from the TopVNC window (WEB tab,
//! specs/018-cloudflare-setup-ui). TopVNC hosts the desktop server itself and
//! supervises the tunnel connector (`cloudflared`) and the web gateway as
//! child processes that never outlive it. This module holds no drawing code.
//!
//! Secrets: the tunnel token is only ever passed to cloudflared as a file
//! path and never read here; the pairing link arrives on the gateway's
//! private stdout pipe, becomes a QR code, and is never shown as text, kept
//! in output, or logged.

pub mod config;
#[cfg(windows)]
mod job;
pub mod lines;
pub mod pairing;
pub mod process;
pub mod runtime;
