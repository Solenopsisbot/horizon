//! JVM class initialization in the interpreter (JVMS §5.5), compiled from
//! real Java at test time.
//!
//! The expected strings are what `java` 27 prints when the same `Probe`
//! methods run on a JVM (`java -cp out demo.Probe` with a `main` that prints
//! each one), not values read back from the interpreter. The refusal case has
//! no JVM counterpart: it pins that a `<clinit>` the interpreter refused never
//! leaves its class half-initialized for a later entrypoint or callback (the
//! shape jamlib's `Event#listeners` hit in a strict game-hook firing).

use horizon_native_java::{NativeError, NativeJar, StdHost, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use zip::write::SimpleFileOptions;

const PROBE: &str = r##"package demo;

public class Probe {
    static String order = "";

    static int mark(String step) {
        order += step + ";";
        return 1;
    }

    static class Boom {
        static int x = 7;
        static {
            if (true) throw new IllegalStateException("bad");
        }
        static int get() { return x; }
    }

    static class ErrBoom {
        static {
            if (true) throw new AssertionError("assert");
        }
        static void touch() {}
    }

    static class Base {
        static { Probe.mark("Base"); }
    }

    static class Sub extends Base {
        static { Probe.mark("Sub"); }
        static void touch() {}
    }

    interface WithDefault {
        int K = Probe.mark("WithDefault");
        default void d() {}
    }

    interface NoDefault {
        int K = Probe.mark("NoDefault");
        void a();
    }

    static class Impl implements NoDefault, WithDefault {
        static { Probe.mark("Impl"); }
        public void a() {}
        static void touch() {}
    }

    static class FailingBase {
        static {
            if (true) throw new RuntimeException("base fails");
        }
    }

    static class SubOfFailing extends FailingBase {
        static void touch() {}
    }

    static class Recursive {
        static int a = 1;
        static int seen = Peek.peek();
        static int b = 2;
    }

    static class Peek {
        static int peek() { return Recursive.a * 10 + Recursive.b; }
    }

    static String describe(Throwable thrown) {
        if (thrown instanceof ExceptionInInitializerError) return "EIIE:" + thrown.getMessage();
        if (thrown instanceof NoClassDefFoundError) return "NCDFE:" + thrown.getMessage();
        if (thrown instanceof AssertionError) return "AssertionError:" + thrown.getMessage();
        return "other";
    }

    /** A throwing initializer: wrapped once, then the class is unusable. */
    public static String failingInitializer() {
        String out = "";
        try { Boom.get(); } catch (Throwable thrown) { out += describe(thrown) + "|"; }
        try { Boom.get(); } catch (Throwable thrown) { out += describe(thrown) + "|"; }
        try { ErrBoom.touch(); } catch (Throwable thrown) { out += describe(thrown) + "|"; }
        try { ErrBoom.touch(); } catch (Throwable thrown) { out += describe(thrown) + "|"; }
        try {
            Boom.get();
            out += "ran";
        } catch (Exception exception) {
            out += "caught-as-exception";
        } catch (Error error) {
            out += "error";
        }
        return out;
    }

    /** Superclass first, then superinterfaces with a default method only. */
    public static String supertypesFirst() {
        Sub.touch();
        String supers = order;
        order = "";
        Impl.touch();
        return supers + "|" + order;
    }

    /** A failing superclass fails the subclass, which stays erroneous. */
    public static String failingSuperclass() {
        String out = "";
        try { SubOfFailing.touch(); } catch (Throwable thrown) { out += describe(thrown) + "|"; }
        try { SubOfFailing.touch(); } catch (Throwable thrown) { out += describe(thrown); }
        return out;
    }

    /** Same-thread re-entry sees the partially initialized class. */
    public static String recursiveInitializer() {
        return "seen=" + Recursive.seen + ",b=" + Recursive.b;
    }
}
"##;

/// An entrypoint whose class initializer writes one static, calls an API the
/// host does not model, then writes another.
const REFUSED: &str = r#"package demo;

import org.example.missing.Api;

public class Refused {
    static String before = "set";
    static {
        Api.call();
    }
    static String after = "set";

    public static String read() {
        return before + "/" + after;
    }
}
"#;

const ENTRY: &str = r#"package demo;

public class Entry {
    public void onInitialize() {
        Refused.read();
    }
}
"#;

/// Compiled for `javac`, left out of the jar: the call reaches the host.
const MISSING_API: &str = r#"package org.example.missing;

public class Api {
    public static void call() {}
}
"#;

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
            "horizon-native-java-class-init-{test}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let mut fixture = Self {
            root,
            jar: NativeJar::default(),
        };
        let classes = fixture.root.join("classes");
        let sources = [
            ("demo/Probe.java", PROBE),
            ("demo/Refused.java", REFUSED),
            ("demo/Entry.java", ENTRY),
            ("org/example/missing/Api.java", MISSING_API),
        ];
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
            "javac failed for class-init fixture {test}"
        );

        let jar_path = fixture.root.join("class-init.jar");
        let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).unwrap());
        zip.start_file("fabric.mod.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            br#"{"id":"class-init-fixture","version":"1.0","entrypoints":{"main":"demo.Entry"}}"#,
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
fn a_throwing_initializer_is_wrapped_once_then_the_class_is_unusable() {
    let fixture = Fixture::build("failing");
    assert_eq!(
        fixture.text("failingInitializer"),
        "EIIE:null|NCDFE:Could not initialize class demo.Probe$Boom|\
         AssertionError:assert|NCDFE:Could not initialize class demo.Probe$ErrBoom|error"
    );
}

#[test]
fn superclass_and_default_method_interfaces_initialize_first() {
    let fixture = Fixture::build("supertypes");
    assert_eq!(
        fixture.text("supertypesFirst"),
        "Base;Sub;|WithDefault;Impl;"
    );
}

#[test]
fn a_failing_superclass_makes_the_subclass_erroneous() {
    let fixture = Fixture::build("failing-super");
    assert_eq!(
        fixture.text("failingSuperclass"),
        "EIIE:null|NCDFE:Could not initialize class demo.Probe$SubOfFailing"
    );
}

#[test]
fn same_thread_reentry_sees_the_partially_initialized_class() {
    let fixture = Fixture::build("recursive");
    assert_eq!(fixture.text("recursiveInitializer"), "seen=10,b=2");
}

#[test]
fn a_refused_initializer_stays_refused_for_later_entrypoints() {
    let fixture = Fixture::build("refused");
    let mut host = fixture
        .jar
        .seed_host(StdHost::new())
        .with_strict_unknown_host_calls();
    let first = fixture
        .jar
        .execute("demo.Entry", None, "main", &mut host)
        .expect_err("the strict host refuses the unmodelled call");
    assert!(
        !matches!(first, NativeError::UncaughtException { .. }),
        "a refusal is not a Java exception a jar could catch: {first}"
    );
    // The class state the first run left behind is the host's now. A second
    // entrypoint touching the class must not see `before` set and `after`
    // unset; it is refused with the first run's cause.
    let second = fixture
        .jar
        .execute("demo.Entry", None, "main", &mut host)
        .expect_err("the erroneous class is never used half-initialized");
    let text = second.to_string();
    assert!(
        text.contains("class demo.Refused failed to initialize earlier")
            && text.contains("org/example/missing/Api"),
        "{text}"
    );
}
