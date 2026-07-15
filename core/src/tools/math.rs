//! The `calculate` tool — one expression in, one exact decimal out.

use crate::{CoreError, Result};
use serde_json::{json, Value};

pub fn calculate(args: &Value) -> Result<(Value, String)> {
    let expr = args
        .get("expression")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CoreError::BadArg("'expression' is required".into()))?;
    let v = crate::mathexpr::eval(expr)?;
    let out = v.normalize().to_string();
    Ok((
        json!({ "expression": expr, "result": out }),
        format!("calculate {expr} = {out}"),
    ))
}
