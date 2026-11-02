//! Sill for Windows: the host side of Sill's protocol, for Windows PCs. `main.rs` is the program;
//! the modules are a library so tests/conformance.rs can check them against Sill's Swift code.

pub mod identity;
pub mod input;
pub mod mdns;
pub mod pairing;
pub mod protocol;
pub mod server;
pub mod tls;
pub mod video;
