//! Vanilla 26.3's `net.minecraft.world.level.block.ColorCollection`.
//!
//! The record holds one value per dye colour, in `DyeColor` order, and mods
//! derive their sixteen coloured block ids from it
//! (`ColorCollection.prefixWithColor(ColorCollection.create("canvas_sign"))
//! .map(...)`, then `white()`... in Farmer's Delight). `map` runs a jar
//! function per colour, so the collection lives in the interpreter: its values
//! are a runtime array, and every member answers from that array exactly as
//! the record's own code would.

use super::*;

const OWNER: &str = "net/minecraft/world/level/block/ColorCollection";
const DESCRIPTOR: &str = "Lnet/minecraft/world/level/block/ColorCollection;";

/// `DyeColor` order: the record's components, `getName()` of each constant
/// and the accessor names.
const COLORS: [(&str, &str, &str); 16] = [
    ("WHITE", "white", "white"),
    ("ORANGE", "orange", "orange"),
    ("MAGENTA", "magenta", "magenta"),
    ("LIGHT_BLUE", "light_blue", "lightBlue"),
    ("YELLOW", "yellow", "yellow"),
    ("LIME", "lime", "lime"),
    ("PINK", "pink", "pink"),
    ("GRAY", "gray", "gray"),
    ("LIGHT_GRAY", "light_gray", "lightGray"),
    ("CYAN", "cyan", "cyan"),
    ("PURPLE", "purple", "purple"),
    ("BLUE", "blue", "blue"),
    ("BROWN", "brown", "brown"),
    ("GREEN", "green", "green"),
    ("RED", "red", "red"),
    ("BLACK", "black", "black"),
];

impl<H: Host> Runtime<'_, H> {
    fn color_collection(&mut self, values: Vec<Value>) -> Value {
        let marker = self.new_array("color-collection", "values");
        self.arrays.insert(marker.clone(), values);
        Value::Ref(Some(marker))
    }

    /// The sixteen values of a collection this runtime built.
    fn color_values(&self, value: &Value) -> Option<Vec<Value>> {
        let marker = expect_ref(value)?;
        if !marker.starts_with("array/color-collection/") {
            return None;
        }
        self.arrays.get(marker).cloned()
    }

    pub(super) fn invoke_color_collection(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if r.owner != OWNER {
            return Ok(None);
        }
        let result = |value: Value| Ok(Some(value));
        match (r.name.as_str(), receiver) {
            ("create", None) if r.descriptor == format!("(Ljava/lang/Object;){DESCRIPTOR}") => {
                let value = args.first().cloned().unwrap_or(Value::Ref(None));
                let collection = self.color_collection(vec![value; 16]);
                result(collection)
            }
            ("prefixWithColor", None) => {
                let Some(values) = args.first().and_then(|value| self.color_values(value)) else {
                    return Ok(None);
                };
                let prefixed = COLORS
                    .iter()
                    .zip(values)
                    .map(|((_, name, _), value)| {
                        Value::Ref(Some(format!("{name}_{}", expect_string(&value))))
                    })
                    .collect();
                let collection = self.color_collection(prefixed);
                result(collection)
            }
            ("zipMap" | "zipApply", None) => {
                let (Some(left), Some(right), Some(function)) = (
                    args.first().and_then(|value| self.color_values(value)),
                    args.get(1).and_then(|value| self.color_values(value)),
                    args.get(2).cloned(),
                ) else {
                    return Ok(None);
                };
                let mut mapped = Vec::with_capacity(16);
                for (left, right) in left.into_iter().zip(right) {
                    let Some(value) = self.invoke_callback(&function, &[left, right])? else {
                        return Ok(None);
                    };
                    mapped.push(value);
                }
                if r.name == "zipApply" {
                    return result(Value::Void);
                }
                let collection = self.color_collection(mapped);
                result(collection)
            }
            (name, Some(receiver)) => {
                let Some(values) = self.color_values(receiver) else {
                    return Ok(None);
                };
                if let Some(index) = COLORS.iter().position(|(_, _, accessor)| *accessor == name)
                    && r.descriptor == "()Ljava/lang/Object;"
                {
                    return result(values[index].clone());
                }
                match name {
                    "pick" => {
                        let color = args
                            .first()
                            .and_then(expect_ref)
                            .cloned()
                            .unwrap_or_default();
                        let constant = color.rsplit('/').next().unwrap_or_default();
                        match COLORS.iter().position(|(name, _, _)| *name == constant) {
                            Some(index) => result(values[index].clone()),
                            None => Ok(None),
                        }
                    }
                    "map" | "forEach" => {
                        let Some(function) = args.first().cloned() else {
                            return Ok(None);
                        };
                        let mut mapped = Vec::with_capacity(16);
                        for value in values {
                            let Some(value) = self.invoke_callback(&function, &[value])? else {
                                return Ok(None);
                            };
                            mapped.push(value);
                        }
                        if name == "forEach" {
                            return result(Value::Void);
                        }
                        let collection = self.color_collection(mapped);
                        result(collection)
                    }
                    "asList" => {
                        let list = self.host.invoke(
                            &MethodRef {
                                owner: "java/util/List".into(),
                                name: "of".into(),
                                descriptor: "([Ljava/lang/Object;)Ljava/util/List;".into(),
                            },
                            &values,
                        )?;
                        result(list)
                    }
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }
}
