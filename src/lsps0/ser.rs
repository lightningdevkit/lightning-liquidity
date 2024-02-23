pub(crate) mod string_amount {
	use crate::prelude::{String, ToString};
	use core::str::FromStr;
	use serde::de::Unexpected;
	use serde::{Deserialize, Deserializer, Serializer};

	pub(crate) fn serialize<S>(x: &u64, s: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		s.serialize_str(&x.to_string())
	}

	pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
	where
		D: Deserializer<'de>,
	{
		let buf = String::deserialize(deserializer)?;

		u64::from_str(&buf).map_err(|_| {
			serde::de::Error::invalid_value(Unexpected::Str(&buf), &"invalid u64 amount string")
		})
	}
}

pub(crate) mod string_amount_option {
	use crate::prelude::{String, ToString};
	use core::str::FromStr;
	use serde::de::Unexpected;
	use serde::{Deserialize, Deserializer, Serialize, Serializer};

	pub(crate) fn serialize<S>(x: &Option<u64>, s: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		let v = x.as_ref().map(|v| v.to_string());
		Option::<String>::serialize(&v, s)
	}

	pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
	where
		D: Deserializer<'de>,
	{
		if let Some(buf) = Option::<String>::deserialize(deserializer)? {
			let val = u64::from_str(&buf).map_err(|_| {
				serde::de::Error::invalid_value(Unexpected::Str(&buf), &"invalid u64 amount string")
			})?;
			Ok(Some(val))
		} else {
			Ok(None)
		}
	}
}
