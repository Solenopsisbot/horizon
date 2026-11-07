//! `Class.isEnum` and `Class.getEnumConstants` over the class-file model.
//!
//! Settings frameworks pick a converter per field type and treat enums as a
//! case of their own (Carpet's `ParsedRule` asks `type.isEnum()` and lists the
//! options from `getEnumConstants()`), so both must answer the JVM's way:
//!
//! - `isEnum` is true exactly when the class's direct superclass is
//!   `java.lang.Enum` (a constant with its own body is a subclass of the enum,
//!   not an enum itself, on the JVM as well);
//! - `getEnumConstants` runs the enum's own `values()`, which is what the JVM
//!   does, so the constants are the objects the enum's `<clinit>` built; a game
//!   enum answers its modelled constants, and a non-enum answers null.

use super::*;

impl<H: Host> Runtime<'_, H> {
    pub(super) fn invoke_reflect_enum(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
    ) -> Result<Option<Value>, NativeError> {
        if r.owner != "java/lang/Class" {
            return Ok(None);
        }
        let is_enum_query = r.name == "isEnum" && r.descriptor == "()Z";
        let constants_query =
            r.name == "getEnumConstants" && r.descriptor == "()[Ljava/lang/Object;";
        if !is_enum_query && !constants_query {
            return Ok(None);
        }
        let class = reflection_class_name(receiver, r)?.replace('.', "/");
        let jar_enum = self
            .class(&class)
            .map(|file| file.super_name == "java/lang/Enum");
        let game_constants = game_enum_constants(&class);
        if is_enum_query {
            return Ok(match (jar_enum, game_constants) {
                (Some(is_enum), _) => Some(Value::Int(i32::from(is_enum))),
                (None, Some(_)) => Some(Value::Int(1)),
                // A class the runtime does not hold keeps the host's answer.
                (None, None) => None,
            });
        }
        match (jar_enum, game_constants) {
            (Some(true), _) => {
                let descriptor = format!("()[L{class};");
                self.invoke(&class, "values", &descriptor, None, &[])
                    .map(Some)
            }
            (Some(false), _) => Ok(Some(Value::Ref(None))),
            (None, Some(constants)) => {
                let marker = self.new_array("java/lang/Class", "getEnumConstants");
                let values = constants
                    .iter()
                    .map(|name| Value::Ref(Some(format!("enum/{class}/{name}"))))
                    .collect();
                self.arrays.insert(marker.clone(), values);
                Ok(Some(Value::Ref(Some(marker))))
            }
            (None, None) => Ok(None),
        }
    }
}
