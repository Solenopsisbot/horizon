//! Vanilla 26.3's `net.minecraft.references.BlockItemId`.
//!
//! The record pairs a block key and an item key; `create` builds both with
//! `ResourceKey.create(Registries.BLOCK/ITEM, id)` and mods register a block
//! under `blockItemId.block()` (Farmer's Delight's `ModBlockItemIds`). The
//! host keeps the two identifiers in the marker and answers each accessor
//! through its own `ResourceKey.create`, so the key is exactly the one a direct
//! `ResourceKey.create` call would have produced, never a nameless null.

use super::*;

const OWNER: &str = "net/minecraft/references/BlockItemId";
const PREFIX: &str = "block-item-id/";
const RESOURCE_KEY_CREATE: &str = "(Lnet/minecraft/resources/ResourceKey;Lnet/minecraft/resources/Identifier;)Lnet/minecraft/resources/ResourceKey;";

fn marker(block: &Value, item: &Value) -> Value {
    Value::Ref(Some(format!(
        "{PREFIX}{},{}",
        value_marker(block),
        value_marker(item)
    )))
}

fn parts(value: &Value) -> Option<(Value, Value)> {
    let rest = expect_ref(value)?.strip_prefix(PREFIX)?;
    let parts = split_capture_markers(rest).ok()?;
    let [block, item] = parts.as_slice() else {
        return None;
    };
    Some((
        parse_value_marker(block).ok()?,
        parse_value_marker(item).ok()?,
    ))
}

impl StdHost {
    /// `BlockItemId.create(...)`; `None` for every other static call.
    pub(super) fn block_item_id_static(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        if r.owner != OWNER || r.name != "create" {
            return None;
        }
        let default_namespace = |value: &Value| {
            let path = expect_string(value);
            identifier_value("minecraft", path)
        };
        Some(Ok(match (r.descriptor.as_str(), args) {
            (
                "(Lnet/minecraft/resources/Identifier;Lnet/minecraft/resources/Identifier;)Lnet/minecraft/references/BlockItemId;",
                [block, item],
            ) => marker(block, item),
            (
                "(Ljava/lang/String;Ljava/lang/String;)Lnet/minecraft/references/BlockItemId;",
                [block, item],
            ) => marker(&default_namespace(block), &default_namespace(item)),
            ("(Ljava/lang/String;)Lnet/minecraft/references/BlockItemId;", [name]) => {
                let id = default_namespace(name);
                marker(&id, &id)
            }
            _ => return None,
        }))
    }

    /// `block()`/`item()` on a modelled id: the key `ResourceKey.create`
    /// builds for that registry and identifier.
    pub(super) fn block_item_id_instance(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
    ) -> Option<Result<Value, NativeError>> {
        if r.owner != OWNER || r.descriptor != "()Lnet/minecraft/resources/ResourceKey;" {
            return None;
        }
        let (block, item) = parts(receiver)?;
        let (registry, id) = match r.name.as_str() {
            "block" => ("BLOCK", block),
            "item" => ("ITEM", item),
            _ => return None,
        };
        let registry = MethodRef {
            owner: "net/minecraft/core/registries/Registries".into(),
            name: registry.into(),
            descriptor: "Lnet/minecraft/resources/ResourceKey;".into(),
        };
        let create = MethodRef {
            owner: "net/minecraft/resources/ResourceKey".into(),
            name: "create".into(),
            descriptor: RESOURCE_KEY_CREATE.into(),
        };
        Some(
            self.get_static(&registry)
                .and_then(|registry| self.invoke(&create, &[registry, id])),
        )
    }
}
