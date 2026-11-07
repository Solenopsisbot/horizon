//! Method handles built at runtime (`method_handles.rs`), compiled from real
//! Java at test time.
//!
//! `converters` is WorldEdit's piston converter pipeline in miniature:
//! `findStatic` of a JDK `valueOf`, `dropArguments`, `asType`,
//! `filterReturnValue`, `catchException`, and a `Converter` lambda spun by a
//! direct `LambdaMetafactory.metafactory` call over `MethodHandles.invoker`.
//! The expected strings are what `java` 27 prints for the same `Probe`
//! methods, not values read back from the interpreter.

use horizon_native_java::{NativeJar, StdHost, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use zip::write::SimpleFileOptions;

const PROBE: &str = r##"package demo;

import java.lang.invoke.LambdaMetafactory;
import java.lang.invoke.MethodHandle;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.MethodType;
import java.lang.invoke.WrongMethodTypeException;
import java.math.BigInteger;

public class Probe {
    public interface Converter {
        Result convert(String text, Object context);
    }

    public static class Result {
        final String text;

        Result(String text) {
            this.text = text;
        }

        public String text() {
            return text;
        }
    }

    public static Result ok(Object value) {
        return new Result("ok:" + value);
    }

    public static Result failed(Throwable thrown) {
        return new Result("failed:" + (thrown instanceof NumberFormatException));
    }

    public static String join(String left, String right) {
        return left + "+" + right;
    }

    static final MethodType CONVERTER_SIG =
        MethodType.methodType(Result.class, String.class, new Class<?>[] {Object.class});
    static final MethodHandle TO_CONVERTER;

    static {
        try {
            MethodHandle invoker = MethodHandles.invoker(CONVERTER_SIG);
            TO_CONVERTER = LambdaMetafactory.metafactory(
                    MethodHandles.lookup(),
                    "convert",
                    MethodType.methodType(Converter.class, MethodHandle.class),
                    CONVERTER_SIG,
                    invoker,
                    CONVERTER_SIG)
                .dynamicInvoker();
        } catch (Throwable thrown) {
            throw new IllegalStateException(thrown);
        }
    }

    /** WorldEdit's piston builds every argument converter this way. */
    static Converter converterFor(Class<?> type) throws Throwable {
        MethodHandle handle = MethodHandles.publicLookup()
            .findStatic(type, "valueOf", MethodType.methodType(type, String.class));
        handle = MethodHandles.dropArguments(handle, 1, new Class<?>[] {Object.class});
        MethodHandle ok = MethodHandles.lookup()
            .findStatic(Probe.class, "ok", MethodType.methodType(Result.class, Object.class));
        handle = MethodHandles.filterReturnValue(
            handle, ok.asType(MethodType.methodType(Result.class, handle.type().returnType())));
        MethodHandle failed = MethodHandles.lookup()
            .findStatic(Probe.class, "failed", MethodType.methodType(Result.class, Throwable.class));
        handle = MethodHandles.catchException(handle, Throwable.class, failed);
        return (Converter) TO_CONVERTER.invokeExact(handle);
    }

    public static String converters() throws Throwable {
        Converter ints = converterFor(Integer.class);
        Converter booleans = converterFor(Boolean.class);
        return ints.convert("42", null).text() + "|" + ints.convert("x", null).text() + "|"
            + booleans.convert("true", null).text();
    }

    public static String lookups() throws Throwable {
        String out = "";
        try {
            MethodHandles.publicLookup()
                .findStatic(Character.class, "valueOf", MethodType.methodType(Character.class, String.class));
            out += "found";
        } catch (NoSuchMethodException missing) {
            out += "no-such-method";
        }
        MethodHandle big = MethodHandles.publicLookup()
            .findConstructor(BigInteger.class, MethodType.methodType(void.class, String.class));
        MethodHandle dropped = MethodHandles.dropArguments(
            MethodHandles.publicLookup().findStatic(
                Integer.class, "valueOf", MethodType.methodType(Integer.class, String.class)),
            1, new Class<?>[] {Object.class});
        return out + "|" + big.type() + "|" + dropped.type() + "|" + dropped.type().parameterCount()
            + "|" + (dropped.type().parameterType(1) == Object.class)
            + "|" + (dropped.type().returnType() == Integer.class);
    }

    public static String binding() throws Throwable {
        MethodHandle join = MethodHandles.lookup().findStatic(
            Probe.class, "join", MethodType.methodType(String.class, String.class, String.class));
        MethodHandle left = MethodHandles.insertArguments(join, 0, new Object[] {"a"});
        MethodHandle right = join.bindTo("b");
        String out = (String) left.invokeExact("z") + "|" + (String) right.invoke("y") + "|"
            + left.type();
        try {
            Object wrong = left.invokeExact("z");
            out += "|exact-ran";
        } catch (WrongMethodTypeException wrong) {
            out += "|wrong-type";
        }
        return out;
    }
}
"##;

fn javac() -> PathBuf {
    std::env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin/javac"))
        .unwrap_or_else(|| "javac".into())
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root)
        .unwrap()
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files_under(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

struct Fixture {
    root: PathBuf,
    jar: NativeJar,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn build(test: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "horizon-native-java-method-handles-{test}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let mut fixture = Self {
            root,
            jar: NativeJar::default(),
        };
        let classes = fixture.root.join("classes");
        let sources = [("demo/Probe.java", PROBE)];
        let mut javac = Command::new(javac());
        javac.args(["--release", "21", "-d"]).arg(&classes);
        for (path, source) in sources {
            let file = fixture.root.join("src").join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, source).unwrap();
            javac.arg(file);
        }
        let status = javac.status().unwrap();
        assert!(
            status.success(),
            "javac failed for method-handle fixture {test}"
        );

        let jar_path = fixture.root.join("method-handles.jar");
        let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).unwrap());
        zip.start_file("fabric.mod.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            br#"{"id":"method-handle-fixture","version":"1.0","entrypoints":{"main":"demo.Probe"}}"#,
        )
        .unwrap();
        for class in files_under(&classes.join("demo")) {
            let entry = class
                .strip_prefix(&classes)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            zip.start_file(entry, SimpleFileOptions::default()).unwrap();
            zip.write_all(&fs::read(class).unwrap()).unwrap();
        }
        zip.finish().unwrap();

        fixture.jar = NativeJar::from_path(&jar_path).unwrap();
        fixture
    }

    fn text(&self, method: &str) -> String {
        let mut host = self.jar.seed_host(StdHost::new());
        match self
            .jar
            .invoke_method_with_args(
                "demo/Probe",
                method,
                "()Ljava/lang/String;",
                None,
                &[],
                &mut host,
            )
            .unwrap_or_else(|error| panic!("{method} failed: {error}"))
        {
            Value::Ref(Some(text)) => text,
            other => panic!("{method} did not return a string: {other:?}"),
        }
    }
}

#[test]
fn a_piston_style_converter_parses_and_catches_like_the_jvm() {
    let fixture = Fixture::build("converters");
    assert_eq!(fixture.text("converters"), "ok:42|failed:true|ok:true");
}

#[test]
fn jdk_lookups_answer_what_the_jdk_has_and_types_follow_combinators() {
    let fixture = Fixture::build("lookups");
    assert_eq!(
        fixture.text("lookups"),
        "no-such-method|(String)BigInteger|(String,Object)Integer|2|true|true"
    );
}

#[test]
fn inserted_and_bound_arguments_and_exact_types() {
    let fixture = Fixture::build("binding");
    assert_eq!(fixture.text("binding"), "a+z|b+y|(String)String|wrong-type");
}
