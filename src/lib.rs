// This file is Copyright its original authors, visible in version contror
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.
#![crate_name = "ldk_lsp_client"]

//! # ldk-lsp-client
//! Types and primitives to integrate a spec-compliant LSP with an LDK-based node.
#![deny(missing_docs)]
#![deny(broken_intra_doc_links)]
#![deny(private_intra_doc_links)]
#![allow(bare_trait_objects)]
#![allow(ellipsis_inclusive_range_patterns)]
#![allow(clippy::drop_non_drop)]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

mod channel_request;
pub mod events;
mod jit_channel;
mod transport;
mod utils;

pub use transport::message_handler::{LiquidityManager, LiquidityProviderConfig};
pub use transport::msgs;
