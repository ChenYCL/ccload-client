#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ccload_client_lib::run();
}
