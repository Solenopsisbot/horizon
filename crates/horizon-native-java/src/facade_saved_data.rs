//! A server's saved data (`SavedDataStorage`) for Java callbacks.
//!
//! Mods keep per-world state as a `SavedData` subclass behind a
//! `SavedDataType(id, constructor, codec, dataFixType)`, reached through
//! `server.getDataStorage()` (or a level's, which in 26.3 is the same one
//! storage under `world/data`). Measured on the corpus' listening jars
//! (7 November 2026, 204 jars on five dedicated servers with fight bots),
//! `getDataStorage` was the most common façade refusal: 8 callbacks in
//! graves, baguettelib, journeymap, MoogsPaths and icy's better horses.
//!
//! The storage answers as vanilla's does on the server thread:
//!
//! - `new SavedDataType(...)` keeps its id, constructor and codec by the
//!   object's marker ([`SavedDataState::init`]); `id()`, `constructor()`,
//!   `codec()`, `dataFixType()`, `equals` and `hashCode` (by id, as the
//!   record's own) answer from them.
//! - `get(type)` is the object `set` or `computeIfAbsent` stored for the
//!   type's id, or `null`; `set(type, data)` stores it.
//! - `computeIfAbsent(type)` is `get`, then (when absent) the type's
//!   constructor supplier run by the interpreter and `set`
//!   ([`Runtime::invoke_saved_data`]), exactly vanilla's order. The
//!   interpreter runs it because the host cannot call back into the jar.
//! - `setDirty()`, `setDirty(boolean)` and `isDirty()` keep the flag of a
//!   `SavedData` object.
//!
//! **Not persisted yet.** Vanilla reads `data/<namespace>/<path>.dat` the
//! first time a type is asked for and writes the dirty ones on save. Both go
//! through the type's `Codec`, which the interpreter holds only as an opaque
//! value, so this storage starts every server empty, as on a new world, and
//! keeps the data for the life of the jar's resident host. A save or a
//! `scheduleSave` call refuses by name rather than pretending to write.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Host, MethodRef, NativeError, Runtime, Value, expect_ref, stable_hash};

/// The one storage marker every `getDataStorage()` answers.
pub(crate) const STORAGE: &str = "game/saved-data/storage";
const STORAGE_CLASS: &str = "net/minecraft/world/level/storage/SavedDataStorage";
const TYPE_CLASS: &str = "net/minecraft/world/level/saveddata/SavedDataType";
const SAVED_DATA_CLASS: &str = "net/minecraft/world/level/saveddata/SavedData";
const TYPE_INIT: &str = "(Lnet/minecraft/resources/Identifier;Ljava/util/function/Supplier;Lcom/mojang/serialization/Codec;Lnet/minecraft/util/datafix/DataFixTypes;)V";
const TYPE_ARG: &str = "(Lnet/minecraft/world/level/saveddata/SavedDataType;)";

/// Saved data objects a jar's storage holds before the first `set` is
/// refused: a bound on a callback that would mint types without end.
const MAX_SAVED_DATA: usize = 4096;

/// One `new SavedDataType(...)`.
#[derive(Clone, Debug, PartialEq)]
struct SavedType {
    /// `namespace:path`.
    id: String,
    id_value: Value,
    constructor: Value,
    codec: Value,
    fix: Value,
}

/// The saved data one jar's resident host holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SavedDataState {
    /// Every `SavedDataType` the jar built, by object marker.
    types: BTreeMap<String, SavedType>,
    /// The stored data by type id.
    data: BTreeMap<String, Value>,
    /// The markers of `SavedData` objects whose dirty flag is set.
    dirty: BTreeSet<String>,
}

fn id_of(value: &Value) -> Option<String> {
    let text = expect_ref(value)?.strip_prefix("identifier/")?;
    Some(if text.contains(':') {
        text.to_owned()
    } else {
        format!("minecraft:{text}")
    })
}

impl SavedDataState {
    /// Every marker this state holds, for the heap's collector: stored data
    /// and the types' suppliers and codecs must outlive the call that built
    /// them.
    pub(crate) fn root_texts<'a>(&'a self, out: &mut Vec<&'a str>) {
        let mut value = |value: &'a Value| {
            if let Value::Ref(Some(text)) = value {
                out.push(text);
            }
        };
        for (marker, saved) in &self.types {
            let _ = marker;
            value(&saved.constructor);
            value(&saved.codec);
            value(&saved.fix);
        }
        self.data.values().for_each(value);
        out.extend(self.types.keys().map(String::as_str));
    }

    /// The type a `SavedDataType` argument names.
    fn type_of(&self, value: &Value) -> Option<&SavedType> {
        self.types.get(expect_ref(value)?)
    }

    /// Answer a call this state owns, or `None` for any other.
    pub(crate) fn call(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let text = expect_ref(receiver)?;
        let name = r.name.as_str();
        let descriptor = r.descriptor.as_str();
        if r.owner == TYPE_CLASS && name == "<init>" {
            if descriptor != TYPE_INIT {
                return Some(Err(NativeError::UnknownHostCall(r.clone())));
            }
            let [id_value, constructor, codec, fix] = args else {
                return Some(Err(NativeError::UnknownHostCall(r.clone())));
            };
            let Some(id) = id_of(id_value) else {
                return Some(Err(NativeError::Metadata(format!(
                    "SavedDataType built with an id the façade cannot read: {id_value:?}"
                ))));
            };
            self.types.insert(
                text.clone(),
                SavedType {
                    id,
                    id_value: id_value.clone(),
                    constructor: constructor.clone(),
                    codec: codec.clone(),
                    fix: fix.clone(),
                },
            );
            return Some(Ok(Value::Void));
        }
        if let Some(saved) = self.types.get(text) {
            return Some(match (name, descriptor) {
                ("id", "()Lnet/minecraft/resources/Identifier;") => Ok(saved.id_value.clone()),
                ("constructor", "()Ljava/util/function/Supplier;") => Ok(saved.constructor.clone()),
                ("codec", "()Lcom/mojang/serialization/Codec;") => Ok(saved.codec.clone()),
                ("dataFixType", "()Lnet/minecraft/util/datafix/DataFixTypes;") => {
                    Ok(saved.fix.clone())
                }
                // The record's `equals`/`hashCode` read the id only.
                ("hashCode", "()I") => Ok(Value::Int(stable_hash(&saved.id))),
                ("equals", "(Ljava/lang/Object;)Z") => {
                    let same = args
                        .first()
                        .and_then(|other| self.type_of(other))
                        .is_some_and(|other| other.id == saved.id);
                    Ok(Value::Int(i32::from(same)))
                }
                ("toString", "()Ljava/lang/String;") => {
                    Ok(Value::Ref(Some(format!("SavedDataType[{}]", saved.id))))
                }
                _ => Err(NativeError::UnknownHostCall(r.clone())),
            });
        }
        if text == STORAGE {
            let saved = args.first().and_then(|arg| self.type_of(arg)).cloned();
            return Some(match (name, saved) {
                ("get", Some(saved)) if descriptor.starts_with(TYPE_ARG) => Ok(self
                    .data
                    .get(&saved.id)
                    .cloned()
                    .unwrap_or(Value::Ref(None))),
                ("set", Some(saved))
                    if descriptor
                        == "(Lnet/minecraft/world/level/saveddata/SavedDataType;Lnet/minecraft/world/level/saveddata/SavedData;)V" =>
                {
                    let Some(data) = args.get(1).filter(|v| **v != Value::Ref(None)) else {
                        return Some(Err(NativeError::Metadata(format!(
                            "SavedDataStorage.set({}) with null data",
                            saved.id
                        ))));
                    };
                    if !self.data.contains_key(&saved.id) && self.data.len() >= MAX_SAVED_DATA {
                        return Some(Err(NativeError::Metadata(format!(
                            "a jar's saved data passed {MAX_SAVED_DATA} types"
                        ))));
                    }
                    self.data.insert(saved.id, data.clone());
                    Ok(Value::Void)
                }
                // `computeIfAbsent` is the interpreter's (it runs the
                // supplier); one reaching here had no type the façade knows.
                _ => Err(NativeError::UnknownHostCall(r.clone())),
            });
        }
        // `SavedData`'s own members on a jar's subclass object.
        let flag = match (name, descriptor) {
            ("setDirty", "()V") => Some(true),
            ("setDirty", "(Z)V") => Some(matches!(args.first(), Some(Value::Int(v)) if *v != 0)),
            ("isDirty", "()Z") => None,
            _ => return None,
        };
        let known = r.owner == SAVED_DATA_CLASS || self.data.values().any(|v| v == receiver);
        if !known {
            return None;
        }
        Some(Ok(match flag {
            Some(true) => {
                self.dirty.insert(text.clone());
                Value::Void
            }
            Some(false) => {
                self.dirty.remove(text);
                Value::Void
            }
            None => Value::Int(i32::from(self.dirty.contains(text))),
        }))
    }
}

/// Whether a call belongs to the saved-data façade: a hook-tier host passes
/// these to the jar's bootstrap host, which keeps the state.
pub(crate) fn owns_call(r: &MethodRef, receiver: Option<&str>) -> bool {
    matches!(
        r.owner.as_str(),
        STORAGE_CLASS | TYPE_CLASS | SAVED_DATA_CLASS
    ) || receiver == Some(STORAGE)
}

impl<H: Host> Runtime<'_, H> {
    /// `SavedDataStorage.computeIfAbsent(type)`: vanilla's `get`, then the
    /// type's constructor and `set` when nothing is stored. `None` for every
    /// other call.
    pub(crate) fn invoke_saved_data(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if r.name != "computeIfAbsent"
            || r.descriptor
                != "(Lnet/minecraft/world/level/saveddata/SavedDataType;)Lnet/minecraft/world/level/saveddata/SavedData;"
        {
            return Ok(None);
        }
        let (Some(storage), [saved_type]) = (receiver, args) else {
            return Ok(None);
        };
        if expect_ref(storage).map(String::as_str) != Some(STORAGE) {
            return Ok(None);
        }
        let call = |name: &str, descriptor: &str, owner: &str| MethodRef {
            owner: owner.into(),
            name: name.into(),
            descriptor: descriptor.into(),
        };
        let get = call(
            "get",
            "(Lnet/minecraft/world/level/saveddata/SavedDataType;)Lnet/minecraft/world/level/saveddata/SavedData;",
            STORAGE_CLASS,
        );
        let current = self
            .host
            .invoke_instance(&get, storage, std::slice::from_ref(saved_type))?;
        if current != Value::Ref(None) {
            return Ok(Some(current));
        }
        let constructor = self.host.invoke_instance(
            &call("constructor", "()Ljava/util/function/Supplier;", TYPE_CLASS),
            saved_type,
            &[],
        )?;
        let Some(created) = self.invoke_callback(&constructor, &[])? else {
            return Err(NativeError::Metadata(format!(
                "SavedDataType constructor is not a callback the interpreter can run: {constructor:?}"
            )));
        };
        self.host.invoke_instance(
            &call(
                "set",
                "(Lnet/minecraft/world/level/saveddata/SavedDataType;Lnet/minecraft/world/level/saveddata/SavedData;)V",
                STORAGE_CLASS,
            ),
            storage,
            &[saved_type.clone(), created.clone()],
        )?;
        Ok(Some(created))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(owner: &str, name: &str, descriptor: &str) -> MethodRef {
        MethodRef {
            owner: owner.into(),
            name: name.into(),
            descriptor: descriptor.into(),
        }
    }

    fn marker(text: &str) -> Value {
        Value::Ref(Some(text.into()))
    }

    fn build(state: &mut SavedDataState, object: &str, id: &str) {
        let init = call(TYPE_CLASS, "<init>", TYPE_INIT);
        let args = [
            marker(&format!("identifier/{id}")),
            marker("@indy:demo/Data::new"),
            marker("codec/opaque"),
            Value::Ref(None),
        ];
        assert_eq!(
            state.call(&init, &marker(object), &args).unwrap().unwrap(),
            Value::Void
        );
    }

    #[test]
    fn get_answers_what_set_stored_for_the_types_id() {
        let mut state = SavedDataState::default();
        // Graves builds a new `SavedDataType` on every call; two objects with
        // one id are one entry, as the record's `equals` makes them.
        build(
            &mut state,
            "net/minecraft/world/level/saveddata/SavedDataType#1",
            "graves:graves",
        );
        build(
            &mut state,
            "net/minecraft/world/level/saveddata/SavedDataType#2",
            "graves:graves",
        );
        let storage = marker(STORAGE);
        let get = call(
            STORAGE_CLASS,
            "get",
            "(Lnet/minecraft/world/level/saveddata/SavedDataType;)Lnet/minecraft/world/level/saveddata/SavedData;",
        );
        let first = marker("net/minecraft/world/level/saveddata/SavedDataType#1");
        let second = marker("net/minecraft/world/level/saveddata/SavedDataType#2");
        assert_eq!(
            state
                .call(&get, &storage, std::slice::from_ref(&first))
                .unwrap()
                .unwrap(),
            Value::Ref(None)
        );
        let set = call(
            STORAGE_CLASS,
            "set",
            "(Lnet/minecraft/world/level/saveddata/SavedDataType;Lnet/minecraft/world/level/saveddata/SavedData;)V",
        );
        state
            .call(
                &set,
                &storage,
                &[first, marker("eu/pb4/graves/GraveManager#4")],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            state.call(&get, &storage, &[second]).unwrap().unwrap(),
            marker("eu/pb4/graves/GraveManager#4")
        );
    }

    #[test]
    fn dirty_flags_follow_set_dirty_and_unknown_members_refuse() {
        let mut state = SavedDataState::default();
        let data = marker("demo/Data#3");
        let dirty = call(SAVED_DATA_CLASS, "isDirty", "()Z");
        assert_eq!(
            state.call(&dirty, &data, &[]).unwrap().unwrap(),
            Value::Int(0)
        );
        state
            .call(&call(SAVED_DATA_CLASS, "setDirty", "()V"), &data, &[])
            .unwrap()
            .unwrap();
        assert_eq!(
            state.call(&dirty, &data, &[]).unwrap().unwrap(),
            Value::Int(1)
        );
        state
            .call(
                &call(SAVED_DATA_CLASS, "setDirty", "(Z)V"),
                &data,
                &[Value::Int(0)],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            state.call(&dirty, &data, &[]).unwrap().unwrap(),
            Value::Int(0)
        );
        // Saving is not modelled: it refuses by name.
        let save = call(STORAGE_CLASS, "saveAndJoin", "()V");
        assert!(state.call(&save, &marker(STORAGE), &[]).unwrap().is_err());
        // A call on an object the state does not know is not its own.
        assert!(
            state
                .call(
                    &call("demo/Data", "setDirty", "()V"),
                    &marker("demo/Other#1"),
                    &[]
                )
                .is_none()
        );
    }
}
