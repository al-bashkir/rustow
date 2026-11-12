//! rustow: a flat-repository dotfiles and configuration manager.

/// `println!` for command results; see [`report::out`].
macro_rules! outln {
    ($($arg:tt)*) => { $crate::report::out(format_args!("{}\n", format_args!($($arg)*))) };
}

pub mod app;
pub mod cache;
pub mod cli;
pub mod complete;
pub mod config;
pub mod crypto;
pub mod diff;
pub mod doctor;
pub mod enc;
pub mod error;
pub mod execute;
pub mod files;
pub mod fold;
pub mod fsops;
pub mod hash;
pub mod ignore;
pub mod init;
pub mod inspect;
pub mod paths;
pub mod plan;
pub mod repo;
pub mod report;
pub mod source;
pub mod state;
pub mod template;
