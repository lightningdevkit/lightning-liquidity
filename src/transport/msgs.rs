use std::{collections::HashMap, convert::TryFrom, fmt};

use lightning::impl_writeable_msg;
use lightning::ln::wire;
use serde::{
	de::{self, MapAccess, Visitor},
	ser::SerializeStruct,
	Deserialize, Deserializer, Serialize,
};

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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Prefix {
	LSPS0,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ResponseError {
	pub code: i32,
	pub message: String,
	pub data: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ListProtocolsRequest {}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ListProtocolsResponse {
	pub protocols: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS0Request {
	ListProtocols(ListProtocolsRequest),
}

impl TryFrom<LSPSRequest> for LSPS0Request {
	type Error = ();

	fn try_from(request: LSPSRequest) -> Result<Self, Self::Error> {
		match request {
			LSPSRequest::LSPS0(_, request) => Ok(request),
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPSRequest {
	LSPS0(RequestId, LSPS0Request),
}

impl LSPSRequest {
	pub fn prefix(&self) -> Prefix {
		match self {
			LSPSRequest::LSPS0(_, _) => Prefix::LSPS0,
		}
	}

	pub fn method(&self) -> &str {
		match self {
			LSPSRequest::LSPS0(_, request) => match request {
				LSPS0Request::ListProtocols(_) => "lsps0.listprotocols",
			},
		}
	}

	pub fn id(&self) -> &str {
		match self {
			LSPSRequest::LSPS0(request_id, _) => &request_id.0,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS0Response {
	ListProtocols(ListProtocolsResponse),
	ListProtocolsError(ResponseError),
}

impl TryFrom<LSPSResponse> for LSPS0Response {
	type Error = ();

	fn try_from(response: LSPSResponse) -> Result<Self, Self::Error> {
		match response {
			LSPSResponse::LSPS0(_, response) => Ok(response),
			LSPSResponse::InvalidRequest(_) => Err(()),
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPSResponse {
	InvalidRequest(ResponseError),
	LSPS0(RequestId, LSPS0Response),
}

impl LSPSResponse {
	pub fn prefix(&self) -> Option<Prefix> {
		match self {
			LSPSResponse::LSPS0(_, _) => Some(Prefix::LSPS0),
			LSPSResponse::InvalidRequest(_) => None,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS0Notification {
	Unknown,
}

impl TryFrom<LSPSNotification> for LSPS0Notification {
	type Error = ();

	fn try_from(notification: LSPSNotification) -> Result<Self, Self::Error> {
		match notification {
			LSPSNotification::LSPS0(notification) => Ok(notification),
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPSNotification {
	LSPS0(LSPS0Notification),
}

impl LSPSNotification {
	pub fn prefix(&self) -> Prefix {
		match self {
			LSPSNotification::LSPS0(_) => Prefix::LSPS0,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPSMessage {
	Request(LSPSRequest),
	Response(LSPSResponse),
	Notification(LSPSNotification),
}

impl LSPSMessage {
	pub fn from_str_with_id_map(
		json_str: &str, request_id_to_method: &HashMap<String, String>,
	) -> Result<Self, serde_json::Error> {
		let deserializer = &mut serde_json::Deserializer::from_str(json_str);
		let visitor = LSPSMessageVisitor { request_id_to_method };
		deserializer.deserialize_any(visitor)
	}
}

impl Serialize for LSPSMessage {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		let mut jsonrpc_object = serializer.serialize_struct("LSPSMessage", 3)?;

		jsonrpc_object.serialize_field("jsonrpc", "2.0")?;

		match self {
			LSPSMessage::Request(request) => {
				jsonrpc_object.serialize_field("method", request.method())?;
				jsonrpc_object.serialize_field("id", request.id())?;

				match request {
					LSPSRequest::LSPS0(_, request) => match request {
						LSPS0Request::ListProtocols(request) => {
							jsonrpc_object.serialize_field("params", request)?
						}
					},
				};
			}
			LSPSMessage::Response(response) => match response {
				LSPSResponse::LSPS0(request_id, response) => {
					jsonrpc_object.serialize_field("id", &request_id.0)?;

					match response {
						LSPS0Response::ListProtocols(result) => {
							jsonrpc_object.serialize_field("result", result)?;
						}
						LSPS0Response::ListProtocolsError(error) => {
							jsonrpc_object.serialize_field("error", error)?;
						}
					}
				}
				LSPSResponse::InvalidRequest(error) => {
					jsonrpc_object.serialize_field("id", &serde_json::Value::Null)?;
					jsonrpc_object.serialize_field("error", error)?;
				}
			},
			LSPSMessage::Notification(notification) => match notification {
				LSPSNotification::LSPS0(notification) => match notification {
					LSPS0Notification::Unknown => {
						jsonrpc_object.serialize_field("method", "lsps0.unknown")?;
					}
				},
			},
		}

		jsonrpc_object.end()
	}
}

struct LSPSMessageVisitor<'a> {
	request_id_to_method: &'a HashMap<String, String>,
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
		let mut method: Option<String> = None;
		let mut params = None;
		let mut result = None;
		let mut error: Option<ResponseError> = None;

		while let Some(key) = map.next_key()? {
			match key {
				"id" => {
					id = Some(map.next_value()?);
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

		if let Some(method) = method {
			if let Some(id) = id {
				match method.as_str() {
					"lsps0.listprotocols" => {
						let list_protocols_request =
							serde_json::from_value(params.unwrap_or_default())
								.map_err(de::Error::custom)?;
						Ok(LSPSMessage::Request(LSPSRequest::LSPS0(
							RequestId(id),
							LSPS0Request::ListProtocols(list_protocols_request),
						)))
					}
					_ => Err(de::Error::custom("Unknown method")),
				}
			} else {
				Ok(LSPSMessage::Notification(LSPSNotification::LSPS0(LSPS0Notification::Unknown)))
			}
		} else if let Some(id) = id {
			if let Some(method) = self.request_id_to_method.get(&id) {
				match method.as_str() {
					"lsps0.listprotocols" => {
						if let Some(error) = error {
							Ok(LSPSMessage::Response(LSPSResponse::LSPS0(
								RequestId(id),
								LSPS0Response::ListProtocolsError(error),
							)))
						} else if let Some(result) = result {
							let list_protocols_response =
								serde_json::from_value(result).map_err(de::Error::custom)?;
							Ok(LSPSMessage::Response(LSPSResponse::LSPS0(
								RequestId(id),
								LSPS0Response::ListProtocols(list_protocols_response),
							)))
						} else {
							Err(de::Error::custom("invalid JSON-RPC object"))
						}
					}
					_ => Err(de::Error::custom("Unknown method")),
				}
			} else {
				Err(de::Error::custom("unknown request id"))
			}
		} else {
			Err(de::Error::custom("invalid JSON-RPC object"))
		}
	}
}

#[cfg(test)]
mod tests {
	// use super::*;

	// #[test]
	// fn deserializes_response() {
	// 	let json = r#"{
	//         "jsonrpc": "2.0",
	//         "id": 1,
	//         "result": {
	//             "foo": "bar"
	//         }
	//     }"#;
	// 	let response: LSPSMessage = serde_json::from_str(json).unwrap();
	// 	assert_eq!(
	// 		response,
	// 		LSPSMessage::Response(jsonrpc::Response::Success(jsonrpc::SuccessResponse {
	// 			jsonrpc: "2.0".to_string(),
	// 			id: serde_json::json!(1),
	// 			result: serde_json::json!({"foo": "bar"}),
	// 		}))
	// 	);
	// }

	// #[test]
	// fn serializes_response() {
	// 	let response =
	// 		LSPSMessage::Response(jsonrpc::Response::Success(jsonrpc::SuccessResponse {
	// 			jsonrpc: "2.0".to_string(),
	// 			id: serde_json::json!(1),
	// 			result: serde_json::json!({"foo": "bar"}),
	// 		}));
	// 	let json = serde_json::to_string(&response).unwrap();
	// 	assert_eq!(json, r#"{"result":{"foo":"bar"},"id":1,"jsonrpc":"2.0"}"#);
	// }
}
