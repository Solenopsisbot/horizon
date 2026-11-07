//! Annotation members as the JVM's annotation proxy answers them.
//!
//! Annotation-driven settings (Carpet's `@Rule`, config libraries'
//! `@Property`) read members the use site never wrote, so the answer is the
//! member's `default` in the annotation interface's `AnnotationDefault`
//! attribute. Members also come in shapes the class-file model stores as
//! strings (enum constants, class literals) or arrays; the member's declared
//! return type says which, so the value is converted against it here:
//!
//! - `String`, primitives: the stored value;
//! - `Class`: the class marker (`Lfoo/Bar;` → `foo/Bar`);
//! - an enum type: the jar enum's constant object, or the `enum/<class>/<NAME>`
//!   marker for an enum outside the jar;
//! - an array of any of these: a runtime-owned array;
//! - a nested annotation: an annotation proxy marker.

use super::*;

impl ClassFile {
    /// The `AnnotationDefault` value of one annotation member, read from the
    /// class bytes on demand like [`ClassFile::field_annotations`].
    fn annotation_default(&self, member: &str) -> Option<AnnotationValue> {
        let mut r = Reader::new(&self.raw);
        if r.u4().ok()? != 0xCAFE_BABE {
            return None;
        }
        r.skip(4).ok()?;
        let pool = read_pool(&mut r).ok()?;
        r.skip(6).ok()?; // access, this, super
        let interfaces = r.u2().ok()? as usize;
        r.skip(interfaces.checked_mul(2)?).ok()?;
        let fields = r.u2().ok()? as usize;
        for _ in 0..fields {
            r.skip(6).ok()?;
            skip_attributes(&mut r, &pool).ok()?;
        }
        let methods = r.u2().ok()? as usize;
        for _ in 0..methods {
            let _access = r.u2().ok()?;
            let name = utf8(&pool, r.u2().ok()?).ok()?;
            let _descriptor = r.u2().ok()?;
            let attributes = r.u2().ok()? as usize;
            for _ in 0..attributes {
                let attribute = utf8(&pool, r.u2().ok()?).ok()?;
                let length = r.u4().ok()? as usize;
                let bytes = r.bytes(length).ok()?;
                if name == member && attribute == "AnnotationDefault" {
                    return read_annotation_value(&mut Reader::new(bytes), &pool).ok();
                }
            }
        }
        None
    }
}

impl<H: Host> Runtime<'_, H> {
    /// The value an annotation member call answers: the use site's value, else
    /// the annotation interface's default, converted to the member's type.
    /// `None` when neither exists (an annotation outside the jar).
    pub(super) fn annotation_member_value(
        &mut self,
        annotation: &Annotation,
        r: &MethodRef,
    ) -> Result<Option<Value>, NativeError> {
        let value = match annotation.values.get(&r.name) {
            Some(value) => value.clone(),
            None => {
                let interface = annotation
                    .descriptor
                    .strip_prefix('L')
                    .and_then(|descriptor| descriptor.strip_suffix(';'))
                    .unwrap_or(&annotation.descriptor);
                match self
                    .class(interface)
                    .and_then(|file| file.annotation_default(&r.name))
                {
                    Some(value) => value,
                    None => return Ok(None),
                }
            }
        };
        self.annotation_value(&value, return_desc(&r.descriptor), annotation, r)
            .map(Some)
    }

    fn annotation_value(
        &mut self,
        value: &AnnotationValue,
        descriptor: &str,
        annotation: &Annotation,
        r: &MethodRef,
    ) -> Result<Value, NativeError> {
        let outside = || {
            NativeError::Metadata(format!(
                "annotation {}#{}{} is outside the bounded annotation value model",
                annotation.descriptor, r.name, r.descriptor
            ))
        };
        Ok(match (value, descriptor) {
            (AnnotationValue::Array(items), element) if element.starts_with('[') => {
                let element = &element[1..];
                let values = items
                    .iter()
                    .map(|item| self.annotation_value(item, element, annotation, r))
                    .collect::<Result<Vec<_>, _>>()?;
                let marker = self.new_array("annotation", &r.name);
                self.arrays.insert(marker.clone(), values);
                Value::Ref(Some(marker))
            }
            (AnnotationValue::String(class), "Ljava/lang/Class;") => {
                // `V` is `void.class`; a primitive keeps its name, as the
                // wrapper `TYPE` markers do.
                Value::Ref(Some(match class.as_str() {
                    "V" => "void".to_owned(),
                    "Z" => "boolean".to_owned(),
                    "B" => "byte".to_owned(),
                    "C" => "char".to_owned(),
                    "S" => "short".to_owned(),
                    "I" => "int".to_owned(),
                    "J" => "long".to_owned(),
                    "F" => "float".to_owned(),
                    "D" => "double".to_owned(),
                    other => other
                        .strip_prefix('L')
                        .and_then(|name| name.strip_suffix(';'))
                        .unwrap_or(other)
                        .to_owned(),
                }))
            }
            (AnnotationValue::String(text), "Ljava/lang/String;") => Value::Ref(Some(text.clone())),
            // Any other reference type holding a string is an enum constant,
            // stored as `Lpkg/Type;.NAME`.
            (AnnotationValue::String(constant), object) if object.starts_with('L') => {
                let (owner, name) = constant.rsplit_once(";.").ok_or_else(outside)?;
                let owner = owner.strip_prefix('L').unwrap_or(owner);
                if self.has_class(owner) {
                    self.ensure_initialized(owner)?;
                    let field = MethodRef {
                        owner: owner.to_owned(),
                        name: name.to_owned(),
                        descriptor: format!("L{owner};"),
                    };
                    self.static_fields
                        .get(&Self::static_field_key(&field))
                        .cloned()
                        .unwrap_or(Value::Ref(None))
                } else {
                    Value::Ref(Some(format!("enum/{owner}/{name}")))
                }
            }
            (AnnotationValue::Bool(value), _) => Value::Int(i32::from(*value)),
            (AnnotationValue::Int(value), _) => Value::Int(*value),
            (AnnotationValue::Long(value), _) => Value::Long(*value),
            (AnnotationValue::Float(value), _) => Value::Float(*value),
            (AnnotationValue::Annotation(nested), _) => {
                self.annotation_proxy(Some((**nested).clone()))
            }
            _ => return Err(outside()),
        })
    }
}
