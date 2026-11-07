//! `java.lang.StackWalker` over the interpreter's own jar frames.
//!
//! Logging shims ask the stack who called them: WorldEdit's
//! `LogManagerCompat.getLogger()` walks `s -> s.skip(2).findFirst()` and names
//! its logger after the class it finds, throwing `AssertionError("No caller
//! found")` when the stream is empty. The interpreter's frames are the only
//! Java frames a native run has, so the walk answers exactly those, innermost
//! first, the way the JVM's walker lists them. Host-side frames (the loader
//! that invoked the entrypoint) do not exist natively and are not invented.
//!
//! The frame list is a thread-local stack pushed and popped by
//! `Runtime::invoke`: one native execution runs on one thread, and a nested
//! runtime (a callback fired from a hook) only adds real callers on top.

use super::*;
use std::cell::RefCell;

thread_local! {
    /// The jar frames currently executing on this thread, outermost first:
    /// `(internal class name, method name)`.
    static FRAMES: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// Marker prefix of one `StackWalker.StackFrame`: `stack-frame/<method>@<class>`.
const FRAME_PREFIX: &str = "stack-frame/";
/// The one walker marker `StackWalker.getInstance` answers.
const WALKER_MARKER: &str = "stack-walker";

/// Record entry into one jar frame. Paired with [`leave`]; the stack never
/// grows past the interpreter's own recursion bound.
pub(crate) fn enter(class: &str, method: &str) {
    FRAMES.with(|frames| {
        frames
            .borrow_mut()
            .push((class.to_owned(), method.to_owned()));
    });
}

/// Record the return (value or error) of the innermost jar frame.
pub(crate) fn leave() {
    FRAMES.with(|frames| {
        frames.borrow_mut().pop();
    });
}

/// The frames a walk sees, innermost first.
pub(crate) fn snapshot() -> Vec<(String, String)> {
    FRAMES.with(|frames| frames.borrow().iter().rev().cloned().collect())
}

fn frame_marker(class: &str, method: &str) -> Value {
    Value::Ref(Some(format!("{FRAME_PREFIX}{method}@{class}")))
}

/// `(class, method)` of a frame marker.
fn parse_frame(value: &Value) -> Option<(&str, &str)> {
    let rest = expect_ref(value)?.strip_prefix(FRAME_PREFIX)?;
    let (method, class) = rest.split_once('@')?;
    Some((class, method))
}

impl<H: Host> Runtime<'_, H> {
    /// Answer the `StackWalker` surface, or `None` for any other call.
    pub(super) fn invoke_stack_walker(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        match (r.owner.as_str(), r.name.as_str()) {
            // Every overload (options, a set of options, an estimated depth)
            // answers the same walker: the frames carry their class either way.
            ("java/lang/StackWalker", "getInstance") => {
                Ok(Some(Value::Ref(Some(WALKER_MARKER.into()))))
            }
            ("java/lang/StackWalker", "walk")
                if r.descriptor == "(Ljava/util/function/Function;)Ljava/lang/Object;" =>
            {
                let function = StdHost::argument(r, args, 0)?.clone();
                // `walk`'s own frame is the JDK's, not the caller's; the first
                // frame the function sees is the method that called `walk`.
                let frames = snapshot()
                    .iter()
                    .map(|(class, method)| frame_marker(class, method))
                    .collect();
                let stream = self.new_stream("java/lang/StackWalker", "walk", frames);
                Ok(Some(
                    self.invoke_callback(&function, &[stream])?
                        .unwrap_or(Value::Ref(None)),
                ))
            }
            ("java/lang/StackWalker", "getCallerClass")
                if r.descriptor == "()Ljava/lang/Class;" =>
            {
                // Frame 0 called `getCallerClass`; its caller is frame 1. With
                // no such frame the JVM throws, as a walk from the bottom of
                // the stack has no caller.
                match snapshot().get(1) {
                    Some((class, _)) => Ok(Some(Value::Ref(Some(class.clone())))),
                    None => Err(NativeError::UncaughtException {
                        class: "java/lang/IllegalCallerException".into(),
                        message: Some("no caller frame".into()),
                        frame: None,
                    }),
                }
            }
            ("java/lang/StackWalker$StackFrame", name) => {
                let Some((class, method)) = receiver.and_then(parse_frame) else {
                    return Ok(None);
                };
                Ok(match name {
                    "getClassName" => Some(Value::Ref(Some(class.replace('/', ".")))),
                    "getMethodName" => Some(Value::Ref(Some(method.to_owned()))),
                    "getDeclaringClass" => Some(Value::Ref(Some(class.to_owned()))),
                    _ => None,
                })
            }
            _ => Ok(None),
        }
    }
}
