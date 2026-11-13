//! The part of the trading examples' shared module (`crates/grid/examples/
//! trading/`) the gateway needs: the instruments, the simulator and its
//! rows. The rest is for windows.
#![allow(dead_code)]

#[path = "../../grid/examples/trading/instruments.rs"]
pub mod instruments;
#[path = "../../grid/examples/trading/market.rs"]
pub mod market;
#[path = "../../grid/examples/trading/rows.rs"]
pub mod rows;
