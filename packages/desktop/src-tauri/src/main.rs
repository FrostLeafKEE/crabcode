#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if crabcode_desktop_lib::run_guest_if_requested() {
        return;
    }
    crabcode_desktop_lib::run();
}
