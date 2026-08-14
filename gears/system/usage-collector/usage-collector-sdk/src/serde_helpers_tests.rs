use std::str::FromStr;

use bigdecimal::BigDecimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::serde_helpers::bigdecimal_str_option;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Holder {
    #[serde(default, with = "bigdecimal_str_option")]
    value: Option<BigDecimal>,
}

#[test]
fn serializes_some_as_plain_decimal_string() {
    let h = Holder {
        value: Some(BigDecimal::from(42)),
    };
    assert_eq!(serde_json::to_value(&h).unwrap(), json!({ "value": "42" }));
}

#[test]
fn serializes_none_as_null() {
    let h = Holder { value: None };
    assert_eq!(serde_json::to_value(&h).unwrap(), json!({ "value": null }));
}

#[test]
fn deserializes_null_as_none() {
    let h: Holder = serde_json::from_value(json!({ "value": null })).unwrap();
    assert_eq!(h.value, None);
}

#[test]
fn round_trips_value_above_rust_decimal_ceiling() {
    // > rust_decimal's ~7.9e28 ceiling (2^96 == 79228162514264337593543950336).
    // This would fail to decode as `Decimal`; it must round-trip exactly as
    // `BigDecimal`.
    let big = "79228162514264337593543950400";
    let h = Holder {
        value: Some(BigDecimal::from_str(big).unwrap()),
    };
    let wire = serde_json::to_value(&h).unwrap();
    assert_eq!(wire, json!({ "value": big }));
    let back: Holder = serde_json::from_value(wire).unwrap();
    assert_eq!(back, h);
}

#[test]
fn serializes_small_value_without_scientific_notation() {
    // `BigDecimal::to_string()` would emit "3.4E-6"; the codec MUST use
    // `to_plain_string()` so the wire stays a plain decimal string.
    let h = Holder {
        value: Some(BigDecimal::from_str("0.0000034").unwrap()),
    };
    assert_eq!(
        serde_json::to_value(&h).unwrap(),
        json!({ "value": "0.0000034" })
    );
}

#[test]
fn rejects_well_typed_but_unparseable_string() {
    // A JSON string is accepted by the type layer, then handed to
    // `BigDecimal::from_str`; garbage must surface as a deserialize error
    // (the `map_err(serde::de::Error::custom)` arm), never a silent accept or
    // a panic.
    let err = serde_json::from_value::<Holder>(json!({ "value": "not-a-number" }))
        .expect_err("an unparseable decimal string MUST be rejected, not silently accepted");
    // Distinct from `rejects_bare_json_number_on_the_wire`: this is a parse
    // failure, not serde's type-mismatch (`invalid type: ...`) arm.
    assert!(
        !err.to_string().contains("invalid type"),
        "error MUST come from the decimal parse, not a type mismatch (got `{err}`)",
    );
}

#[test]
fn rejects_bare_json_number_on_the_wire() {
    let err = serde_json::from_value::<Holder>(json!({ "value": 42 }))
        .expect_err("a bare JSON number MUST be rejected; the wire is string-only");
    assert!(
        err.to_string().contains("string"),
        "error MUST indicate a string was expected (got `{err}`)",
    );
}
