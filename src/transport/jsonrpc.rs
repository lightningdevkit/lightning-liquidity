// This file is Copyright its original authors, visible in version contror
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Types and primitives for [JSON-RPC 2.0 specification](https://www.jsonrpc.org/specification).
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;

pub const VERSION: &str = "2.0";
pub const INVALID_REQUEST_ERROR_CODE: i32 = -32600;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Prefix {
	LSPS0,
	LSPS1,
	LSPS2,
}

impl FromStr for Prefix {
	type Err = ParseMethodError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"lsp0" => Ok(Prefix::LSPS0),
			"lsp1" => Ok(Prefix::LSPS1),
			"lsp2" => Ok(Prefix::LSPS2),
			_ => Err(ParseMethodError),
		}
	}
}

impl Display for Prefix {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Prefix::LSPS0 => write!(f, "lsps0"),
			Prefix::LSPS1 => write!(f, "lsps1"),
			Prefix::LSPS2 => write!(f, "lsps2"),
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Method {
	pub prefix: Prefix,
	pub name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseMethodError;

impl Display for ParseMethodError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "invalid method format")
	}
}

impl FromStr for Method {
	type Err = ParseMethodError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let mut split = s.split('.');
		let prefix = split.next().ok_or(ParseMethodError)?.parse()?;
		let name = split.next().ok_or(ParseMethodError)?.to_string();
		Ok(Method { prefix, name })
	}
}

impl Display for Method {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}.{}", self.prefix, self.name)
	}
}

impl Serialize for Method {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		serializer.serialize_str(&self.to_string())
	}
}

// implement Deserialize for Method using it's FromStr implementation
impl<'de> Deserialize<'de> for Method {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let s = String::deserialize(deserializer)?;
		Method::from_str(&s).map_err(serde::de::Error::custom)
	}
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Error {
	/// A number indicating the error type that occurred.
	pub code: i32,
	/// A string providing a short description of the error.
	pub message: String,
	/// A primitive or structured value that contains additional information about the error.
	pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// A JSONRPC request object.
pub struct Request {
	/// The name of the RPC call.
	pub method: Method,
	/// Parameters to the RPC call.
	/// Note: Only by-name parameters are allowed.
	pub params: HashMap<String, Value>,
	/// Identifier for this Request, which should appear in the response.
	pub id: Value,
	/// jsonrpc field, MUST be "2.0".
	pub jsonrpc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Notification {
	/// The name of the RPC call.
	pub method: Method,
	/// Parameters to the RPC call.
	pub params: HashMap<String, Value>,
	/// jsonrpc field, MUST be "2.0".
	pub jsonrpc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
/// A JSON-RPC 2.0 response object.
pub enum Response {
	/// A successful response.
	Success(SuccessResponse),
	/// An error response.
	Error(ErrorResponse),
}

impl Response {
	pub fn get_id(&self) -> &Value {
		match self {
			Response::Success(success) => &success.id,
			Response::Error(error) => &error.id,
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// A JSON-RPC 2.0 error response object.
pub struct ErrorResponse {
	/// The error.
	pub error: Error,
	/// Identifier for this Request, which should match that of the request.
	pub id: Value,
	/// jsonrpc field, MUST be "2.0".
	pub jsonrpc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// A JSON-RPC 2.0 successful response object.
pub struct SuccessResponse {
	/// The result.
	pub result: Value,
	/// Identifier for this Request, which should match that of the request.
	pub id: Value,
	/// jsonrpc field, MUST be "2.0".
	pub jsonrpc: String,
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn deserializes_error_response() {
		let json = r#"{
            "jsonrpc": "2.0",
            "error": {
                "code": -32600,
                "message": "Invalid Request"
            },
            "id": null
        }"#;

		let response: Response = serde_json::from_str(json).unwrap();
		assert_eq!(
			response,
			Response::Error(ErrorResponse {
				error: Error { code: -32600, message: "Invalid Request".to_string(), data: None },
				id: serde_json::Value::Null,
				jsonrpc: "2.0".to_string(),
			})
		);
	}

	#[test]
	fn deserializes_success_response() {
		let json = r#"{
            "jsonrpc": "2.0",
            "result": 5,
            "id": "1"
        }"#;

		let response: Response = serde_json::from_str(json).unwrap();
		assert_eq!(
			response,
			Response::Success(SuccessResponse {
				result: json!(5),
				id: json!("1"),
				jsonrpc: "2.0".to_string(),
			})
		);
	}

	#[test]
	fn fails_deserialize_with_both_error_result() {
		let json = r#"{
            "jsonrpc": "2.0",
            "result": 5,
            "error": {
                "code": -32600,
                "message": "Invalid Request"
            },
            "id": "1"
        }"#;

		let response: Result<Response, serde_json::Error> = serde_json::from_str(json);
		assert!(response.is_err());
	}

	#[test]
	fn serializes_error_response() {
		let response = Response::Error(ErrorResponse {
			error: Error { code: -32600, message: "Invalid Request".to_string(), data: None },
			id: serde_json::Value::Null,
			jsonrpc: "2.0".to_string(),
		});

		let json = serde_json::to_string(&response).unwrap();
		assert_eq!(
			json,
			r#"{"error":{"code":-32600,"message":"Invalid Request","data":null},"id":null,"jsonrpc":"2.0"}"#
		);
	}

	#[test]
	fn serializes_success_response() {
		let response = Response::Success(SuccessResponse {
			result: json!(5),
			id: json!("1"),
			jsonrpc: "2.0".to_string(),
		});

		let json = serde_json::to_string(&response).unwrap();
		assert_eq!(json, r#"{"result":5,"id":"1","jsonrpc":"2.0"}"#);
	}

	#[test]
	fn deserializes_request_method() {
		let json = r#"{
			"jsonrpc": "2.0",
			"method": "prefix.name",
			"params": { "a": 5 },
			"id": 3
		}"#;

		let request: Request = serde_json::from_str(json).unwrap();
		assert_eq!(
			request,
			Request {
				method: Method { prefix: Prefix::LSPS0, name: "name".to_string() },
				params: vec![("a".to_string(), json!(5)),].into_iter().collect(),
				id: json!(3),
				jsonrpc: "2.0".to_string(),
			}
		);
	}

	#[test]
	fn serializes_request_method() {
		let request = Request {
			method: Method { prefix: Prefix::LSPS0, name: "name".to_string() },
			params: vec![("a".to_string(), json!(5))].into_iter().collect(),
			id: json!(3),
			jsonrpc: "2.0".to_string(),
		};

		let json = serde_json::to_string(&request).unwrap();
		assert_eq!(json, r#"{"method":"prefix.name","params":{"a":5},"id":3,"jsonrpc":"2.0"}"#);
	}
}
