//! Desktop entry point; the app is the library's [`ps_learning_harness_lib::run`].

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ps_learning_harness_lib::run();
}
