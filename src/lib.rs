//! A modern, memory-safe rewrite of Apache httpd's `rotatelogs`.
//!
//! The binary reads log records from stdin (the way httpd's piped logging
//! works), writes them to a rotating set of files, and supports time-based and
//! size-based rotation.
//!
//! The crate is split into small modules so the rotation logic can be tested
//! without spawning the binary or depending on the wall clock.

#![forbid(unsafe_code)]

pub mod cli;
pub mod config;
pub mod error;
pub mod link;
pub mod maintenance;
pub mod naming;
pub mod program;
pub mod runner;
pub mod writer;

pub use error::{Error, Result};
