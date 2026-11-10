//! The browser application entry point.
//!
//! In M0 this only prints the version. The window, tabs and URL bar arrive in M1.

fn main() {
    println!(
        "web_browser {} (M0 skeleton: nothing to browse yet)",
        env!("CARGO_PKG_VERSION")
    );
}
