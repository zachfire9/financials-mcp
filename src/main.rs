//! The `financials-mcp` executable.
//!
//! Kept deliberately thin: it only calls into the library crate
//! (`src/lib.rs`), where the logic can be unit tested.

fn main() {
    println!("{}", financials_mcp::banner());
}
