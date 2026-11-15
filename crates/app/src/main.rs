//! Desktop entry point. The shell (lib.rs) is compiled into this binary
//! rather than linked from the library target, which Android and iOS load
//! (see Cargo.toml).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[path = "lib.rs"]
mod shell;

fn main() {
    shell::run();
}
