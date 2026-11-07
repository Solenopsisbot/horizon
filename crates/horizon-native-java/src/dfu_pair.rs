//! DataFixerUpper's `com.mojang.datafixers.util.Pair`, which the game ships.
//!
//! Config trees pair a sort key with a reflected field (`Pair.of(ordinal,
//! field)`, then `pair.getSecond()`), so the pair must hand back exactly the
//! values it was built from. The marker keeps both values encoded like a
//! `Map.Entry` marker: `dfu-pair/<first>,<second>`.

use super::*;

const PREFIX: &str = "dfu-pair/";
const OWNER: &str = "com/mojang/datafixers/util/Pair";

fn pair_value(first: &Value, second: &Value) -> Value {
    Value::Ref(Some(format!(
        "{PREFIX}{},{}",
        value_marker(first),
        value_marker(second)
    )))
}

fn pair_parts(value: &Value) -> Result<(Value, Value), NativeError> {
    let marker = expect_ref(value)
        .and_then(|marker| marker.strip_prefix(PREFIX))
        .ok_or_else(|| NativeError::Metadata("expected a native Pair marker".into()))?;
    let parts = split_capture_markers(marker)?;
    let [first, second] = parts.as_slice() else {
        return Err(NativeError::Metadata(
            "Pair marker must hold two values".into(),
        ));
    };
    Ok((parse_value_marker(first)?, parse_value_marker(second)?))
}

/// `Pair.of(first, second)`; `None` for any other static call.
pub(crate) fn static_call(r: &MethodRef, args: &[Value]) -> Option<Result<Value, NativeError>> {
    if r.owner != OWNER
        || r.name != "of"
        || r.descriptor != "(Ljava/lang/Object;Ljava/lang/Object;)Lcom/mojang/datafixers/util/Pair;"
    {
        return None;
    }
    let [first, second] = args else {
        return None;
    };
    Some(Ok(pair_value(first, second)))
}

/// The accessors of a pair the host built; `None` for anything else, so an
/// unmodelled member keeps the neutral answer.
pub(crate) fn instance_call(
    r: &MethodRef,
    receiver: &Value,
    args: &[Value],
) -> Option<Result<Value, NativeError>> {
    if r.owner != OWNER || !expect_ref(receiver).is_some_and(|marker| marker.starts_with(PREFIX)) {
        return None;
    }
    let (first, second) = match pair_parts(receiver) {
        Ok(parts) => parts,
        Err(error) => return Some(Err(error)),
    };
    Some(Ok(match (r.name.as_str(), r.descriptor.as_str()) {
        ("getFirst", "()Ljava/lang/Object;") => first,
        ("getSecond", "()Ljava/lang/Object;") => second,
        ("swap", "()Lcom/mojang/datafixers/util/Pair;") => pair_value(&second, &first),
        ("equals", "(Ljava/lang/Object;)Z") => Value::Int(i32::from(
            args.first()
                .and_then(|other| pair_parts(other).ok())
                .is_some_and(|other| other == (first, second)),
        )),
        _ => return None,
    }))
}
