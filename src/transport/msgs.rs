use crate::transport::jsonrpc;
use lightning::impl_writeable_msg;
use lightning::ln::wire;
use serde::{Deserialize, Serialize};

pub const LSPS_MESSAGE_TYPE: u16 = 37913;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawLSPSMessage {
	pub payload: String,
}

impl_writeable_msg!(RawLSPSMessage, { payload }, {});

impl wire::Type for RawLSPSMessage {
	fn type_id(&self) -> u16 {
		LSPS_MESSAGE_TYPE
	}
}

/// An [`lsps0_message`] message to be sent to or received from a peer.
///
/// [`lsps0_message`]: https://github.com/BitcoinAndLightningLayerSpecs/lsp/blob/49544d1a420cf32ef4726c526a29aa46a09202ff/LSPS0/README.md
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LSPSMessage {
	Request(jsonrpc::Request),
	Response(jsonrpc::Response),
	Notification(jsonrpc::Notification),
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn deserializes_response() {
		let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "foo": "bar"
            }
        }"#;
		let response: LSPSMessage = serde_json::from_str(json).unwrap();
		assert_eq!(
			response,
			LSPSMessage::Response(jsonrpc::Response::Success(jsonrpc::SuccessResponse {
				jsonrpc: "2.0".to_string(),
				id: serde_json::json!(1),
				result: serde_json::json!({"foo": "bar"}),
			}))
		);
	}

	#[test]
	fn serializes_response() {
		let response =
			LSPSMessage::Response(jsonrpc::Response::Success(jsonrpc::SuccessResponse {
				jsonrpc: "2.0".to_string(),
				id: serde_json::json!(1),
				result: serde_json::json!({"foo": "bar"}),
			}));
		let json = serde_json::to_string(&response).unwrap();
		assert_eq!(json, r#"{"result":{"foo":"bar"},"id":1,"jsonrpc":"2.0"}"#);
	}
}
