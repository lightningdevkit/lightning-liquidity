//! Contains basic data types that allow for the (de-)seralization of LSPS messages in the JSON-RPC 2.0 format.
//!
//! Please refer to the [LSPS0 specification](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS0) for more information.

#[cfg(lsps1)]
use crate::lsps1::msgs::{
	LSPS1Message, LSPS1Request, LSPS1Response, LSPS1_CREATE_ORDER_METHOD_NAME,
	LSPS1_GET_INFO_METHOD_NAME, LSPS1_GET_ORDER_METHOD_NAME,
};
use crate::lsps2::msgs::{
	LSPS2Message, LSPS2Request, LSPS2Response, LSPS2_BUY_METHOD_NAME, LSPS2_GET_INFO_METHOD_NAME,
};
use crate::prelude::{HashMap, String, ToString, Vec};

use lightning::impl_writeable_msg;
use lightning::ln::msgs::LightningError;
use lightning::ln::wire;

use bitcoin::secp256k1::PublicKey;

use serde::de;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

use core::convert::TryFrom;
use core::fmt;

const LSPS_MESSAGE_SERIALIZED_STRUCT_NAME: &str = "LSPSMessage";
const JSONRPC_FIELD_KEY: &str = "jsonrpc";
const JSONRPC_FIELD_VALUE: &str = "2.0";
const JSONRPC_METHOD_FIELD_KEY: &str = "method";
const JSONRPC_ID_FIELD_KEY: &str = "id";
const JSONRPC_PARAMS_FIELD_KEY: &str = "params";
const JSONRPC_RESULT_FIELD_KEY: &str = "result";
const JSONRPC_ERROR_FIELD_KEY: &str = "error";
const JSONRPC_INVALID_MESSAGE_ERROR_CODE: i32 = -32700;
const JSONRPC_INVALID_MESSAGE_ERROR_MESSAGE: &str = "parse error";
const LSPS0_LISTPROTOCOLS_METHOD_NAME: &str = "lsps0.list_protocols";

/// The Lightning message type id for LSPS messages.
pub const LSPS_MESSAGE_TYPE_ID: u16 = 37913;

/// A trait used to implement a specific LSPS protocol.
///
/// The messages the protocol uses need to be able to be mapped
/// from and into [`LSPSMessage`].
pub(crate) trait ProtocolMessageHandler {
	type ProtocolMessage: TryFrom<LSPSMessage> + Into<LSPSMessage>;
	const PROTOCOL_NUMBER: Option<u16>;

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError>;
}

/// Lightning message type used by LSPS protocols.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawLSPSMessage {
	/// The raw string payload that holds the actual message.
	pub payload: String,
}

impl_writeable_msg!(RawLSPSMessage, { payload }, {});

impl wire::Type for RawLSPSMessage {
	fn type_id(&self) -> u16 {
		LSPS_MESSAGE_TYPE_ID
	}
}

/// A JSON-RPC request's `id`.
///
/// Please refer to the [JSON-RPC 2.0 specification](https://www.jsonrpc.org/specification#request_object) for
/// more information.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(pub String);

/// An error returned in response to an JSON-RPC request.
///
/// Please refer to the [JSON-RPC 2.0 specification](https://www.jsonrpc.org/specification#error_object) for
/// more information.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ResponseError {
	/// A number that indicates the error type that occurred.
	pub code: i32,
	/// A string providing a short description of the error.
	pub message: String,
	/// A primitive or structured value that contains additional information about the error.
	pub data: Option<String>,
}

/// A `list_protocols` request.
///
/// Please refer to the [LSPS0 specification](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS0#lsps-specification-support-query)
/// for more information.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, Default)]
pub struct ListProtocolsRequest {}

/// A response to a `list_protocols` request.
///
/// Please refer to the [LSPS0 specification](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS0#lsps-specification-support-query)
/// for more information.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ListProtocolsResponse {
	/// A list of supported protocols.
	pub protocols: Vec<u16>,
}

/// An LSPS0 protocol request.
///
/// Please refer to the [LSPS0 specification](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS0)
/// for more information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS0Request {
	/// A request calling `list_protocols`.
	ListProtocols(ListProtocolsRequest),
}

impl LSPS0Request {
	/// Returns the method name associated with the given request variant.
	pub fn method(&self) -> &str {
		match self {
			LSPS0Request::ListProtocols(_) => LSPS0_LISTPROTOCOLS_METHOD_NAME,
		}
	}
}

/// An LSPS0 protocol request.
///
/// Please refer to the [LSPS0 specification](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS0)
/// for more information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS0Response {
	/// A response to a `list_protocols` request.
	ListProtocols(ListProtocolsResponse),
	/// An error response to a `list_protocols` request.
	ListProtocolsError(ResponseError),
}

/// An LSPS0 protocol message.
///
/// Please refer to the [LSPS0 specification](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS0)
/// for more information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS0Message {
	/// A request variant.
	Request(RequestId, LSPS0Request),
	/// A response variant.
	Response(RequestId, LSPS0Response),
}

impl TryFrom<LSPSMessage> for LSPS0Message {
	type Error = ();

	fn try_from(message: LSPSMessage) -> Result<Self, Self::Error> {
		match message {
			LSPSMessage::Invalid => Err(()),
			LSPSMessage::LSPS0(message) => Ok(message),
			#[cfg(lsps1)]
			LSPSMessage::LSPS1(_) => Err(()),
			LSPSMessage::LSPS2(_) => Err(()),
		}
	}
}

impl From<LSPS0Message> for LSPSMessage {
	fn from(message: LSPS0Message) -> Self {
		LSPSMessage::LSPS0(message)
	}
}

/// A (de-)serializable LSPS message allowing to be sent over the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPSMessage {
	/// An invalid variant.
	Invalid,
	/// An LSPS0 message.
	LSPS0(LSPS0Message),
	/// An LSPS1 message.
	#[cfg(lsps1)]
	LSPS1(LSPS1Message),
	/// An LSPS2 message.
	LSPS2(LSPS2Message),
}

impl LSPSMessage {
	/// A constructor returning an `LSPSMessage` from a raw JSON string.
	///
	/// The given `request_id_to_method` associates request ids with method names, as response objects
	/// don't carry the latter.
	pub fn from_str_with_id_map(
		json_str: &str, request_id_to_method_map: &mut HashMap<String, String>,
	) -> Result<Self, serde_json::Error> {
		let deserializer = &mut serde_json::Deserializer::from_str(json_str);
		let visitor = LSPSMessageVisitor { request_id_to_method_map };
		deserializer.deserialize_any(visitor)
	}

	/// Returns the request id and the method.
	pub fn get_request_id_and_method(&self) -> Option<(String, String)> {
		match self {
			LSPSMessage::LSPS0(LSPS0Message::Request(request_id, request)) => {
				Some((request_id.0.clone(), request.method().to_string()))
			}
			#[cfg(lsps1)]
			LSPSMessage::LSPS1(LSPS1Message::Request(request_id, request)) => {
				Some((request_id.0.clone(), request.method().to_string()))
			}
			LSPSMessage::LSPS2(LSPS2Message::Request(request_id, request)) => {
				Some((request_id.0.clone(), request.method().to_string()))
			}
			_ => None,
		}
	}
}

impl Serialize for LSPSMessage {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		let mut jsonrpc_object =
			serializer.serialize_struct(LSPS_MESSAGE_SERIALIZED_STRUCT_NAME, 3)?;

		jsonrpc_object.serialize_field(JSONRPC_FIELD_KEY, JSONRPC_FIELD_VALUE)?;

		match self {
			LSPSMessage::LSPS0(LSPS0Message::Request(request_id, request)) => {
				jsonrpc_object.serialize_field(JSONRPC_METHOD_FIELD_KEY, request.method())?;
				jsonrpc_object.serialize_field(JSONRPC_ID_FIELD_KEY, &request_id.0)?;

				match request {
					LSPS0Request::ListProtocols(params) => {
						jsonrpc_object.serialize_field(JSONRPC_PARAMS_FIELD_KEY, params)?
					}
				};
			}
			LSPSMessage::LSPS0(LSPS0Message::Response(request_id, response)) => {
				jsonrpc_object.serialize_field(JSONRPC_ID_FIELD_KEY, &request_id.0)?;

				match response {
					LSPS0Response::ListProtocols(result) => {
						jsonrpc_object.serialize_field(JSONRPC_RESULT_FIELD_KEY, result)?;
					}
					LSPS0Response::ListProtocolsError(error) => {
						jsonrpc_object.serialize_field(JSONRPC_ERROR_FIELD_KEY, error)?;
					}
				}
			}
			#[cfg(lsps1)]
			LSPSMessage::LSPS1(LSPS1Message::Request(request_id, request)) => {
				jsonrpc_object.serialize_field(JSONRPC_ID_FIELD_KEY, &request_id.0)?;
				jsonrpc_object.serialize_field(JSONRPC_METHOD_FIELD_KEY, request.method())?;

				match request {
					LSPS1Request::GetInfo(params) => {
						jsonrpc_object.serialize_field(JSONRPC_PARAMS_FIELD_KEY, params)?
					}
					LSPS1Request::CreateOrder(params) => {
						jsonrpc_object.serialize_field(JSONRPC_PARAMS_FIELD_KEY, params)?
					}
					LSPS1Request::GetOrder(params) => {
						jsonrpc_object.serialize_field(JSONRPC_PARAMS_FIELD_KEY, params)?
					}
				}
			}
			#[cfg(lsps1)]
			LSPSMessage::LSPS1(LSPS1Message::Response(request_id, response)) => {
				jsonrpc_object.serialize_field(JSONRPC_ID_FIELD_KEY, &request_id.0)?;

				match response {
					LSPS1Response::GetInfo(result) => {
						jsonrpc_object.serialize_field(JSONRPC_RESULT_FIELD_KEY, result)?
					}
					LSPS1Response::CreateOrder(result) => {
						jsonrpc_object.serialize_field(JSONRPC_ERROR_FIELD_KEY, result)?
					}
					LSPS1Response::CreateOrderError(error) => {
						jsonrpc_object.serialize_field(JSONRPC_RESULT_FIELD_KEY, error)?
					}
					LSPS1Response::GetOrder(result) => {
						jsonrpc_object.serialize_field(JSONRPC_ERROR_FIELD_KEY, result)?
					}
					LSPS1Response::GetOrderError(error) => {
						jsonrpc_object.serialize_field(JSONRPC_ERROR_FIELD_KEY, &error)?
					}
				}
			}
			LSPSMessage::LSPS2(LSPS2Message::Request(request_id, request)) => {
				jsonrpc_object.serialize_field(JSONRPC_ID_FIELD_KEY, &request_id.0)?;
				jsonrpc_object.serialize_field(JSONRPC_METHOD_FIELD_KEY, request.method())?;

				match request {
					LSPS2Request::GetInfo(params) => {
						jsonrpc_object.serialize_field(JSONRPC_PARAMS_FIELD_KEY, params)?
					}
					LSPS2Request::Buy(params) => {
						jsonrpc_object.serialize_field(JSONRPC_PARAMS_FIELD_KEY, params)?
					}
				}
			}
			LSPSMessage::LSPS2(LSPS2Message::Response(request_id, response)) => {
				jsonrpc_object.serialize_field(JSONRPC_ID_FIELD_KEY, &request_id.0)?;

				match response {
					LSPS2Response::GetInfo(result) => {
						jsonrpc_object.serialize_field(JSONRPC_RESULT_FIELD_KEY, result)?
					}
					LSPS2Response::GetInfoError(error) => {
						jsonrpc_object.serialize_field(JSONRPC_ERROR_FIELD_KEY, error)?
					}
					LSPS2Response::Buy(result) => {
						jsonrpc_object.serialize_field(JSONRPC_RESULT_FIELD_KEY, result)?
					}
					LSPS2Response::BuyError(error) => {
						jsonrpc_object.serialize_field(JSONRPC_ERROR_FIELD_KEY, error)?
					}
				}
			}
			LSPSMessage::Invalid => {
				let error = ResponseError {
					code: JSONRPC_INVALID_MESSAGE_ERROR_CODE,
					message: JSONRPC_INVALID_MESSAGE_ERROR_MESSAGE.to_string(),
					data: None,
				};

				jsonrpc_object.serialize_field(JSONRPC_ID_FIELD_KEY, &serde_json::Value::Null)?;
				jsonrpc_object.serialize_field(JSONRPC_ERROR_FIELD_KEY, &error)?;
			}
		}

		jsonrpc_object.end()
	}
}

struct LSPSMessageVisitor<'a> {
	request_id_to_method_map: &'a mut HashMap<String, String>,
}

impl<'de, 'a> Visitor<'de> for LSPSMessageVisitor<'a> {
	type Value = LSPSMessage;

	fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
		formatter.write_str("JSON-RPC object")
	}

	fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
	where
		A: MapAccess<'de>,
	{
		let mut id: Option<String> = None;
		let mut method: Option<&str> = None;
		let mut params = None;
		let mut result = None;
		let mut error: Option<ResponseError> = None;

		while let Some(key) = map.next_key()? {
			match key {
				"id" => {
					id = map.next_value()?;
				}
				"method" => {
					method = Some(map.next_value()?);
				}
				"params" => {
					params = Some(map.next_value()?);
				}
				"result" => {
					result = Some(map.next_value()?);
				}
				"error" => {
					error = Some(map.next_value()?);
				}
				_ => {
					let _: serde_json::Value = map.next_value()?;
				}
			}
		}

		let id = id.ok_or_else(|| {
			if let Some(method) = method {
				de::Error::custom(format!("Received unknown notification: {}", method))
			} else {
				de::Error::custom("Received invalid JSON-RPC object: one of method or id required")
			}
		})?;

		match method {
			Some(method) => match method {
				LSPS0_LISTPROTOCOLS_METHOD_NAME => Ok(LSPSMessage::LSPS0(LSPS0Message::Request(
					RequestId(id),
					LSPS0Request::ListProtocols(ListProtocolsRequest {}),
				))),
				#[cfg(lsps1)]
				LSPS1_GET_INFO_METHOD_NAME => {
					let request = serde_json::from_value(params.unwrap_or(json!({})))
						.map_err(de::Error::custom)?;
					Ok(LSPSMessage::LSPS1(LSPS1Message::Request(
						RequestId(id),
						LSPS1Request::GetInfo(request),
					)))
				}
				#[cfg(lsps1)]
				LSPS1_CREATE_ORDER_METHOD_NAME => {
					let request = serde_json::from_value(params.unwrap_or(json!({})))
						.map_err(de::Error::custom)?;
					Ok(LSPSMessage::LSPS1(LSPS1Message::Request(
						RequestId(id),
						LSPS1Request::CreateOrder(request),
					)))
				}
				#[cfg(lsps1)]
				LSPS1_GET_ORDER_METHOD_NAME => {
					let request = serde_json::from_value(params.unwrap_or(json!({})))
						.map_err(de::Error::custom)?;
					Ok(LSPSMessage::LSPS1(LSPS1Message::Request(
						RequestId(id),
						LSPS1Request::GetOrder(request),
					)))
				}
				LSPS2_GET_INFO_METHOD_NAME => {
					let request = serde_json::from_value(params.unwrap_or(json!({})))
						.map_err(de::Error::custom)?;
					Ok(LSPSMessage::LSPS2(LSPS2Message::Request(
						RequestId(id),
						LSPS2Request::GetInfo(request),
					)))
				}
				LSPS2_BUY_METHOD_NAME => {
					let request = serde_json::from_value(params.unwrap_or(json!({})))
						.map_err(de::Error::custom)?;
					Ok(LSPSMessage::LSPS2(LSPS2Message::Request(
						RequestId(id),
						LSPS2Request::Buy(request),
					)))
				}
				_ => Err(de::Error::custom(format!(
					"Received request with unknown method: {}",
					method
				))),
			},
			None => match self.request_id_to_method_map.remove(&id) {
				Some(method) => match method.as_str() {
					LSPS0_LISTPROTOCOLS_METHOD_NAME => {
						if let Some(error) = error {
							Ok(LSPSMessage::LSPS0(LSPS0Message::Response(
								RequestId(id),
								LSPS0Response::ListProtocolsError(error),
							)))
						} else if let Some(result) = result {
							let list_protocols_response =
								serde_json::from_value(result).map_err(de::Error::custom)?;
							Ok(LSPSMessage::LSPS0(LSPS0Message::Response(
								RequestId(id),
								LSPS0Response::ListProtocols(list_protocols_response),
							)))
						} else {
							Err(de::Error::custom("Received invalid JSON-RPC object: one of method, result, or error required"))
						}
					}
					#[cfg(lsps1)]
					LSPS1_CREATE_ORDER_METHOD_NAME => {
						if let Some(error) = error {
							Ok(LSPSMessage::LSPS1(LSPS1Message::Response(
								RequestId(id),
								LSPS1Response::CreateOrderError(error),
							)))
						} else if let Some(result) = result {
							let response =
								serde_json::from_value(result).map_err(de::Error::custom)?;
							Ok(LSPSMessage::LSPS1(LSPS1Message::Response(
								RequestId(id),
								LSPS1Response::CreateOrder(response),
							)))
						} else {
							Err(de::Error::custom("Received invalid JSON-RPC object: one of method, result, or error required"))
						}
					}
					#[cfg(lsps1)]
					LSPS1_GET_ORDER_METHOD_NAME => {
						if let Some(error) = error {
							Ok(LSPSMessage::LSPS1(LSPS1Message::Response(
								RequestId(id),
								LSPS1Response::GetOrderError(error),
							)))
						} else if let Some(result) = result {
							let response =
								serde_json::from_value(result).map_err(de::Error::custom)?;
							Ok(LSPSMessage::LSPS1(LSPS1Message::Response(
								RequestId(id),
								LSPS1Response::GetOrder(response),
							)))
						} else {
							Err(de::Error::custom("Received invalid JSON-RPC object: one of method, result, or error required"))
						}
					}
					LSPS2_GET_INFO_METHOD_NAME => {
						if let Some(error) = error {
							Ok(LSPSMessage::LSPS2(LSPS2Message::Response(
								RequestId(id),
								LSPS2Response::GetInfoError(error),
							)))
						} else if let Some(result) = result {
							let response =
								serde_json::from_value(result).map_err(de::Error::custom)?;
							Ok(LSPSMessage::LSPS2(LSPS2Message::Response(
								RequestId(id),
								LSPS2Response::GetInfo(response),
							)))
						} else {
							Err(de::Error::custom("Received invalid JSON-RPC object: one of method, result, or error required"))
						}
					}
					LSPS2_BUY_METHOD_NAME => {
						if let Some(error) = error {
							Ok(LSPSMessage::LSPS2(LSPS2Message::Response(
								RequestId(id),
								LSPS2Response::BuyError(error),
							)))
						} else if let Some(result) = result {
							let response =
								serde_json::from_value(result).map_err(de::Error::custom)?;
							Ok(LSPSMessage::LSPS2(LSPS2Message::Response(
								RequestId(id),
								LSPS2Response::Buy(response),
							)))
						} else {
							Err(de::Error::custom("Received invalid JSON-RPC object: one of method, result, or error required"))
						}
					}
					_ => Err(de::Error::custom(format!(
						"Received response for an unknown request method: {}",
						method
					))),
				},
				None => Err(de::Error::custom(format!(
					"Received response for unknown request id: {}",
					id
				))),
			},
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn deserializes_request() {
		let json = r#"{
			"jsonrpc": "2.0",
			"id": "request:id:xyz123",
			"method": "lsps0.list_protocols"
		}"#;

		let mut request_id_method_map = HashMap::new();

		let msg = LSPSMessage::from_str_with_id_map(json, &mut request_id_method_map);
		assert!(msg.is_ok());
		let msg = msg.unwrap();
		assert_eq!(
			msg,
			LSPSMessage::LSPS0(LSPS0Message::Request(
				RequestId("request:id:xyz123".to_string()),
				LSPS0Request::ListProtocols(ListProtocolsRequest {})
			))
		);
	}

	#[test]
	fn serializes_request() {
		let request = LSPSMessage::LSPS0(LSPS0Message::Request(
			RequestId("request:id:xyz123".to_string()),
			LSPS0Request::ListProtocols(ListProtocolsRequest {}),
		));
		let json = serde_json::to_string(&request).unwrap();
		assert_eq!(
			json,
			r#"{"jsonrpc":"2.0","method":"lsps0.list_protocols","id":"request:id:xyz123","params":{}}"#
		);
	}

	#[test]
	fn deserializes_success_response() {
		let json = r#"{
	        "jsonrpc": "2.0",
	        "id": "request:id:xyz123",
	        "result": {
	            "protocols": [1,2,3]
	        }
	    }"#;
		let mut request_id_to_method_map = HashMap::new();
		request_id_to_method_map
			.insert("request:id:xyz123".to_string(), "lsps0.list_protocols".to_string());

		let response =
			LSPSMessage::from_str_with_id_map(json, &mut request_id_to_method_map).unwrap();

		assert_eq!(
			response,
			LSPSMessage::LSPS0(LSPS0Message::Response(
				RequestId("request:id:xyz123".to_string()),
				LSPS0Response::ListProtocols(ListProtocolsResponse { protocols: vec![1, 2, 3] })
			))
		);
	}

	#[test]
	fn deserializes_error_response() {
		let json = r#"{
	        "jsonrpc": "2.0",
	        "id": "request:id:xyz123",
	        "error": {
	            "code": -32617,
				"message": "Unknown Error"
	        }
	    }"#;
		let mut request_id_to_method_map = HashMap::new();
		request_id_to_method_map
			.insert("request:id:xyz123".to_string(), "lsps0.list_protocols".to_string());

		let response =
			LSPSMessage::from_str_with_id_map(json, &mut request_id_to_method_map).unwrap();

		assert_eq!(
			response,
			LSPSMessage::LSPS0(LSPS0Message::Response(
				RequestId("request:id:xyz123".to_string()),
				LSPS0Response::ListProtocolsError(ResponseError {
					code: -32617,
					message: "Unknown Error".to_string(),
					data: None
				})
			))
		);
	}

	#[test]
	fn deserialize_fails_with_unknown_request_id() {
		let json = r#"{
	        "jsonrpc": "2.0",
	        "id": "request:id:xyz124",
	        "result": {
	            "protocols": [1,2,3]
	        }
	    }"#;
		let mut request_id_to_method_map = HashMap::new();
		request_id_to_method_map
			.insert("request:id:xyz123".to_string(), "lsps0.list_protocols".to_string());

		let response = LSPSMessage::from_str_with_id_map(json, &mut request_id_to_method_map);
		assert!(response.is_err());
	}

	#[test]
	fn serializes_response() {
		let response = LSPSMessage::LSPS0(LSPS0Message::Response(
			RequestId("request:id:xyz123".to_string()),
			LSPS0Response::ListProtocols(ListProtocolsResponse { protocols: vec![1, 2, 3] }),
		));
		let json = serde_json::to_string(&response).unwrap();
		assert_eq!(
			json,
			r#"{"jsonrpc":"2.0","id":"request:id:xyz123","result":{"protocols":[1,2,3]}}"#
		);
	}
}
