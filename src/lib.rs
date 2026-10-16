//! trUSDX Linux audio / CAT driver library.
//!
//! Modules are split by concern (filename = responsibility):
//! - [`audio`] — PulseAudio sink/source, sample convert, RX/TX bridge loop
//! - [`cat_codec`] — pure CAT parse/encode (no I/O)
//! - [`cli`] — args, TTY ESC, terminal meters
//! - [`gui`] — Slint window
//! - [`rigctl`] — hamlib-compatible TCP protocol
//! - [`serial_io`] — soft CAT queue + serial RX demux (CAT text vs US audio)
//! - [`shutdown`] — coordinated stop
//! - [`state`] — shared atomics / queues
//! - [`trusdx`] — radio serial protocol helpers

pub mod audio;
pub mod cat_codec;
pub mod cli;
pub mod gui;
pub mod rigctl;
pub mod serial_io;
pub mod shutdown;
pub mod state;
pub mod trusdx;
