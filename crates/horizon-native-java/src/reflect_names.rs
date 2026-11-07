//! `Class.getSimpleName` for nested jar classes.
//!
//! The JVM takes a nested class's simple name from its `InnerClasses`
//! attribute: `Internals$EntryStackProvider` is `EntryStackProvider`, and an
//! anonymous class has the empty name. REI binds its service suppliers to
//! fields named after the interface's simple name, so answering
//! `Internals$EntryStackProvider` matched no field. A top-level class (no
//! entry for itself) keeps the host's answer, the text after the package.

use super::*;

impl ClassFile {
    /// The simple name this class's own `InnerClasses` entry declares:
    /// `Some(Some(name))` for a member or local class, `Some(None)` for an
    /// anonymous one, `None` when the class is not nested (or the table is
    /// unreadable).
    fn nested_simple_name(&self) -> Option<Option<String>> {
        let mut r = Reader::new(&self.raw);
        if r.u4().ok()? != 0xCAFE_BABE {
            return None;
        }
        r.skip(4).ok()?;
        let pool = read_pool(&mut r).ok()?;
        r.skip(2).ok()?; // access
        let this = r.u2().ok()?;
        r.skip(2).ok()?; // super
        let interfaces = r.u2().ok()? as usize;
        r.skip(interfaces.checked_mul(2)?).ok()?;
        for _ in 0..2 {
            // fields, then methods
            let count = r.u2().ok()? as usize;
            for _ in 0..count {
                r.skip(6).ok()?;
                skip_attributes(&mut r, &pool).ok()?;
            }
        }
        let attributes = r.u2().ok()? as usize;
        for _ in 0..attributes {
            let name = utf8(&pool, r.u2().ok()?).ok()?;
            let length = r.u4().ok()? as usize;
            let bytes = r.bytes(length).ok()?;
            if name != "InnerClasses" {
                continue;
            }
            let mut table = Reader::new(bytes);
            let entries = table.u2().ok()? as usize;
            for _ in 0..entries {
                let inner = table.u2().ok()?;
                let _outer = table.u2().ok()?;
                let simple = table.u2().ok()?;
                let _flags = table.u2().ok()?;
                if inner == this {
                    return Some(if simple == 0 {
                        None
                    } else {
                        Some(utf8(&pool, simple).ok()?)
                    });
                }
            }
        }
        None
    }
}

impl<H: Host> Runtime<'_, H> {
    pub(super) fn invoke_reflect_simple_name(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
    ) -> Result<Option<Value>, NativeError> {
        if r.owner != "java/lang/Class"
            || r.name != "getSimpleName"
            || r.descriptor != "()Ljava/lang/String;"
        {
            return Ok(None);
        }
        let class = reflection_class_name(receiver, r)?.replace('.', "/");
        Ok(self
            .class(&class)
            .and_then(ClassFile::nested_simple_name)
            .map(|simple| Value::Ref(Some(simple.unwrap_or_default()))))
    }
}
