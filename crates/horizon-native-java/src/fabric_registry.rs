//! Mod-defined registries: `ResourceKey.createRegistryKey` and Fabric's
//! `FabricRegistryBuilder`.
//!
//! A library that adds its own registry builds the key, hands it to
//! `FabricRegistryBuilder.create*(key)...buildAndRegister()`, and binds the
//! result before anything reads it (Collector's Album's card types). The host
//! models a registry and its key as the same `registry/<id>` marker (see
//! `BuiltInRegistries`), so the key, the builder and the built registry all
//! carry that id, and the registry the builder answers is the one later
//! `Registry.register` calls name.

use super::*;

const BUILDER: &str = "net/fabricmc/fabric/api/event/registry/FabricRegistryBuilder";
const BUILDER_PREFIX: &str = "fabric-registry-builder/";

/// The `<namespace>:<path>` an identifier or registry marker names.
fn registry_id(value: &Value) -> Option<String> {
    let marker = expect_ref(value)?;
    if let Some(id) = marker.strip_prefix("registry/") {
        return Some(id.to_owned());
    }
    marker.starts_with("identifier/").then(|| {
        let (namespace, path) = identifier_parts(value);
        format!("{namespace}:{path}")
    })
}

pub(crate) fn static_call(r: &MethodRef, args: &[Value]) -> Option<Result<Value, NativeError>> {
    match (r.owner.as_str(), r.name.as_str()) {
        ("net/minecraft/resources/ResourceKey", "createRegistryKey")
            if r.descriptor
                == "(Lnet/minecraft/resources/Identifier;)Lnet/minecraft/resources/ResourceKey;" =>
        {
            let id = registry_id(args.first()?)?;
            Some(Ok(Value::Ref(Some(format!("registry/{id}")))))
        }
        (BUILDER, "create" | "createSimple" | "createDefaulted" | "from")
            if r.descriptor.ends_with(&format!(")L{BUILDER};")) =>
        {
            let id = registry_id(args.first()?)?;
            Some(Ok(Value::Ref(Some(format!("{BUILDER_PREFIX}{id}")))))
        }
        _ => None,
    }
}

pub(crate) fn instance_call(r: &MethodRef, receiver: &Value) -> Option<Result<Value, NativeError>> {
    if r.owner != BUILDER {
        return None;
    }
    let id = expect_ref(receiver)?
        .strip_prefix(BUILDER_PREFIX)?
        .to_owned();
    match r.name.as_str() {
        // Fluent setters answer the builder itself.
        _ if r.descriptor.ends_with(&format!(")L{BUILDER};")) => Some(Ok(receiver.clone())),
        "buildAndRegister" => Some(Ok(Value::Ref(Some(format!("registry/{id}"))))),
        _ => None,
    }
}
