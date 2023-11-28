// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Implementation of LSPS2: JIT Channel Negotiation specification.

pub(crate) mod channel_manager;
pub(crate) mod event;
/// Message, request, and other primitive types used to implement LSPS2.
pub mod msgs;
pub(crate) mod utils;

pub use event::LSPS2Event;
pub use msgs::{BuyResponse, GetInfoResponse, OpeningFeeParams, RawOpeningFeeParams};
pub use utils::compute_opening_fee;
