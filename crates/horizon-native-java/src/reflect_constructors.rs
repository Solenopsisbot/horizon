//! `Class.getDeclaredConstructors` / `getConstructors` over the class file.
//!
//! Config libraries find a type's one-argument and no-argument constructors by
//! listing them and checking `getParameterCount()` (Armor Hider's
//! `ConfigurationItemFactoryRegistry`), so the list must be the class's real
//! `<init>` methods. Each entry is the `reflect-constructor/<class>#<descriptor>`
//! marker `getDeclaredConstructor` already hands out, so `getParameterCount`
//! and `newInstance` answer it unchanged. A class the runtime does not hold
//! keeps the host's answer.

use super::*;

impl<H: Host> Runtime<'_, H> {
    pub(super) fn invoke_reflect_constructors(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
    ) -> Result<Option<Value>, NativeError> {
        if r.owner != "java/lang/Class"
            || !matches!(
                r.name.as_str(),
                "getDeclaredConstructors" | "getConstructors"
            )
            || r.descriptor != "()[Ljava/lang/reflect/Constructor;"
        {
            return Ok(None);
        }
        let class = reflection_class_name(receiver, r)?.replace('.', "/");
        let public_only = r.name == "getConstructors";
        let Some(file) = self.class(&class) else {
            return Ok(None);
        };
        let constructors = file
            .methods
            .iter()
            .filter(|method| method.name == "<init>" && (!public_only || method.access & 1 != 0))
            .map(|method| {
                Value::Ref(Some(format!(
                    "reflect-constructor/{class}#{}",
                    method.descriptor
                )))
            })
            .collect();
        let marker = self.new_array("java/lang/reflect/Constructor", &r.name);
        self.arrays.insert(marker.clone(), constructors);
        Ok(Some(Value::Ref(Some(marker))))
    }
}
