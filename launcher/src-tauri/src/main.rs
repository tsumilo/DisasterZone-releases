// no console window behind the launcher on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    dz_launcher_lib::run()
}
