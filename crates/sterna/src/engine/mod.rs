//! The engine: a session as something any client can drive
//! (`docs/engine.md`). The session thread speaks to the [`hub`]; every
//! client -- the terminal in process, the desktop app and the tests over the
//! [`port`] -- speaks the [`wire`]'s commands and events, and nothing else.

pub mod client;
pub mod data;
pub mod host;
pub mod hub;
pub mod list;
pub mod port;
pub mod reading;
pub mod wire;
