//! Regressions the wave-2 library models exposed on the corpus ledger: shapes
//! that used to pass because the interpreter answered a neutral value, and
//! failed once a model acted on that value the way the JVM would.
//!
//! - FarmersDelight refabricated: `Preconditions.checkState(o instanceof
//!   String)` on its own string default ("Config defaults are invalid"),
//!   because a plain string was not an instance of `String`.
//! - architectury: `EventFactory.createLoop(T... empty)` reads its event
//!   interface from `empty.getClass().getComponentType()`, which was null, and
//!   the null reached `Proxy.newProxyInstance` as an NPE.
//! - tcdcommons: `Path.of(System.getProperty("user.dir"), "config", name)`
//!   with no `user.dir`, an NPE in `Path.of`.
//!
//! The probe is ordinary Java compiled at test time; each expected string is
//! what `java` prints for the same code (Guava's `checkState` stub below
//! throws as Guava does; the jar carries only `demo/`, so the runtime's own
//! Guava model answers it).

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

import com.google.common.base.Preconditions;
import java.lang.reflect.Proxy;
import java.nio.file.Path;
import java.util.List;
import java.util.function.Predicate;

public class Probe {
    static final Predicate<Object> STRING_CHECK = o -> o instanceof String;

    public interface Listener { String fire(String name); }

    static final class StringValue {
        final String value;
        StringValue(String value, Predicate<Object> validator) {
            this.value = value;
            Preconditions.checkState(validator.test(value), "Config defaults are invalid");
        }
    }

    static String kind(Object value) {
        if (value instanceof String text) return "string:" + text.length();
        if (value instanceof Integer number) return "int:" + number;
        if (value instanceof List<?> list) return "list:" + list.size();
        return "other";
    }

    public static String strings() {
        Object text = "farmersdelight:rope";
        Object colour = "#ff0000";
        CharSequence chars = "abc";
        StringValue rope = new StringValue("farmersdelight:rope", STRING_CHECK);
        boolean refused;
        try { new StringValue(null, STRING_CHECK); refused = false; } catch (IllegalStateException e) { refused = true; }
        return (text instanceof String) + "|" + (colour instanceof String) + "|" + (chars instanceof Comparable)
            + "|" + String.class.isInstance(text) + "|" + rope.value + "|" + refused
            + "|" + kind("ab") + "," + kind(7) + "," + kind(List.of(1, 2)) + "," + kind(new Object());
    }

    @SafeVarargs
    static <T> Class<?> componentOf(T... empty) {
        return empty.getClass().getComponentType();
    }

    public static String arrays() {
        Class<?> listener = Probe.<Listener>componentOf();
        Listener proxy = (Listener) Proxy.newProxyInstance(Probe.class.getClassLoader(), new Class<?>[] {listener},
            (self, method, args) -> method.getName() + ":" + args[0]);
        return listener.getName() + "|" + new Listener[0].getClass().getName() + "|" + new int[2].getClass().getName()
            + "|" + new String[1][1].getClass().getName() + "|" + new String[1][1].getClass().getComponentType().getName()
            + "|" + (String.class.getComponentType() == null) + "|" + proxy.fire("tick");
    }

    static String ran = "";

    public static String platformProxy() {
        Class<?> runnable = Probe.<Runnable>componentOf();
        Runnable proxy = (Runnable) Proxy.newProxyInstance(Probe.class.getClassLoader(), new Class<?>[] {runnable},
            (self, method, args) -> { ran += method.getName() + (args == null) + ";"; return null; });
        proxy.run();
        proxy.run();
        return runnable.getName() + "|" + (proxy instanceof Runnable) + "|" + ran;
    }

    public static String workingDirectory() {
        String dir = System.getProperty("user.dir");
        Path config = Path.of(dir, "config", "tcdcommons.json");
        return (dir != null) + "|" + (System.getProperty("user.home") != null) + "|"
            + config.toString().endsWith("/config/tcdcommons.json") + "|" + config.toString().startsWith(dir);
    }
}
"##;

const PRECONDITIONS: &str = "package com.google.common.base;\npublic final class Preconditions {\n    public static void checkState(boolean ok, Object message) { if (!ok) throw new IllegalStateException(String.valueOf(message)); }\n}\n";

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

/// Compile the probe, pack its `demo/` classes into a jar and run
/// `demo.Probe.<method>()`, answering the string it returns.
fn run(test: &str, method: &str) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-wave2-{test}-{}-{unique}",
        std::process::id()
    ));
    let classes = root.join("classes");
    let mut javac = Command::new(javac());
    javac.args(["--release", "21", "-d"]).arg(&classes);
    for (path, source) in [
        ("demo/Probe.java", PROBE),
        ("com/google/common/base/Preconditions.java", PRECONDITIONS),
    ] {
        let file = root.join("src").join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, source).unwrap();
        javac.arg(file);
    }
    assert!(javac.status().unwrap().success(), "javac failed for {test}");
    let jar_path = root.join("probe.jar");
    let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).unwrap());
    zip.start_file("fabric.mod.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(br#"{"id":"wave2-fixture","version":"1.0","entrypoints":{"main":"demo.Probe"}}"#)
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
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let _ = fs::remove_dir_all(&root);
    let mut host = jar.seed_host(StdHost::new());
    match jar
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

#[test]
fn a_plain_string_is_an_instance_of_string_and_passes_a_config_check() {
    assert_eq!(
        run("strings", "strings"),
        "true|true|true|true|farmersdelight:rope|true|string:2,int:7,list:2,other"
    );
}

#[test]
fn an_array_names_its_component_class_and_a_proxy_can_implement_it() {
    assert_eq!(
        run("arrays", "arrays"),
        "demo.Probe$Listener|[Ldemo.Probe$Listener;|[I|[[Ljava.lang.String;|[Ljava.lang.String;\
         |true|fire:tick"
    );
}

#[test]
fn the_working_directory_is_set_so_a_config_path_resolves() {
    assert_eq!(run("workdir", "workingDirectory"), "true|true|true|true");
}

#[test]
fn a_proxy_implements_a_platform_interface_the_class_model_does_not_hold() {
    // Architectury's `EventFactory.createLoop()` over `Runnable` listeners.
    assert_eq!(
        run("platform-proxy", "platformProxy"),
        "java.lang.Runnable|true|runtrue;runtrue;"
    );
}
