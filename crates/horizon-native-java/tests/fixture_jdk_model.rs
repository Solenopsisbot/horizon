//! JDK and logging-library modelling a mod's config bootstrap reaches,
//! compiled from real Java at test time.
//!
//! Each probe method is ordinary Java the JVM answers one way; the expected
//! strings below are what `java` prints for the same code, not values read
//! back from the interpreter. The cases come from real refused jars:
//! NightConfig's class-typed calls into interface defaults and its own
//! `Reader` subclass (TerraBlender), `Collectors.toMap` (Legacy4J), a
//! spliterator-backed service stream (rrls), a null charset or logger in a
//! builder's `requireNonNull` (servercore, Xaero's maps), and a mod's own
//! service type with no providers (main-menu-credits).
//!
//! The logging stubs are compiled for `javac` but left out of the jar: the jar
//! must reach the host's logger model, as a real mod reaches the platform's.

use horizon_native_java::{LinkedClassTable, NativeJar, StdHost, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use zip::write::SimpleFileOptions;

const LOG4J_MANAGER: &str = r#"package org.apache.logging.log4j;
public class LogManager {
    public static Logger getLogger(Class<?> type) { return null; }
}
"#;

const LOG4J_LOGGER: &str = r#"package org.apache.logging.log4j;
public interface Logger {
    String getName();
    boolean isDebugEnabled();
    void info(String message, Object argument);
}
"#;

const SLF4J_FACTORY: &str = r#"package org.slf4j;
public class LoggerFactory {
    public static Logger getLogger(String name) { return null; }
}
"#;

const SLF4J_LOGGER: &str = r#"package org.slf4j;
public interface Logger {
    String getName();
    boolean isInfoEnabled();
    void warn(String message);
}
"#;

const GSON: &str = r#"package com.google.gson;
public class Gson {
    public Gson() {}
    public void toJson(Object src, java.lang.reflect.Type typeOfSrc, Appendable writer) {}
}
"#;

const PROBE: &str = r##"package demo;

import java.io.IOException;
import java.io.Reader;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.ServiceLoader;
import java.util.Spliterators;
import java.util.function.Consumer;
import java.util.function.Supplier;
import java.util.Optional;
import java.util.stream.Collectors;
import java.util.stream.IntStream;
import java.util.stream.StreamSupport;

interface Parser {
    default String parse(String text) { return "parsed:" + text; }
}

final class TomlLikeParser implements Parser {}

interface Shape {
    String name();
}

abstract class BaseShape implements Shape {
    String describe() { return "shape:" + name(); }
}

final class Square extends BaseShape {
    public String name() { return "square"; }
}

/** A jar `Reader` over a string, the shape of NightConfig's `FastStringReader`. */
final class TextReader extends Reader {
    private final String text;
    private int cursor;
    TextReader(String text) { this.text = text; }
    @Override public int read() { return cursor < text.length() ? text.charAt(cursor++) : -1; }
    @Override public int read(char[] buffer, int offset, int length) { return -1; }
    @Override public void close() {}
}

/** Counts 0, 1, 2 through `tryAdvance`, like rrls's service spliterator. */
final class Counting extends Spliterators.AbstractSpliterator<Integer> {
    private int next;
    Counting() { super(Long.MAX_VALUE, 0); }
    @Override public boolean tryAdvance(Consumer<? super Integer> action) {
        if (next >= 3) return false;
        action.accept(next++);
        return true;
    }
}

interface Integration {}

/** rrls's service shape: an interface method the provider class implements. */
interface Ranked {
    boolean active();
    default int priority() { return 0; }
}

final class Low implements Ranked {
    public boolean active() { return true; }
    public int priority() { return -5; }
}

final class High implements Ranked {
    public boolean active() { return true; }
    public int priority() { return 7; }
}

final class Off implements Ranked {
    public boolean active() { return false; }
    public int priority() { return 100; }
}

interface Marker {}

public final class Probe {
    public static String defaults() {
        return new TomlLikeParser().parse("x") + "|" + new Square().describe();
    }

    public static String readerSubclass() throws IOException {
        Reader reader = new TextReader("abc");
        StringBuilder out = new StringBuilder();
        int c;
        while ((c = reader.read()) != -1) {
            out.append((char) c);
        }
        return out.toString();
    }

    public static String instanceOf() {
        Object number = Integer.valueOf(3);
        Object list = new ArrayList<String>();
        Object map = new HashMap<String, Integer>();
        Object flag = Boolean.valueOf(true);
        return (number instanceof Number) + "|" + (number instanceof Integer) + "|"
            + (number instanceof String) + "|" + (list instanceof List) + "|"
            + (list instanceof Map) + "|" + (map instanceof Map) + "|"
            + (flag instanceof Boolean) + "|" + (flag instanceof Number);
    }

    public static String charSequence() {
        CharSequence text = "hello";
        StringBuilder builder = new StringBuilder("ab");
        CharSequence built = builder;
        return text.length() + "|" + text.charAt(1) + "|" + built.length() + "|"
            + text.subSequence(1, 3);
    }

    public static String toMap() {
        Map<String, Integer> lengths = List.of("a", "bb", "ccc").stream()
            .collect(Collectors.toMap(s -> s, String::length));
        String duplicate;
        try {
            List.of("x", "x").stream().collect(Collectors.toMap(s -> s, s -> 1));
            duplicate = "no-throw";
        } catch (IllegalStateException error) {
            duplicate = "duplicate";
        }
        Map<String, Integer> merged = List.of("x", "y", "x").stream()
            .collect(Collectors.toMap(s -> s, s -> 1, Integer::sum));
        return lengths.size() + "|" + lengths.get("bb") + "|" + duplicate + "|"
            + merged.get("x") + "|" + merged.get("y");
    }

    public static String spliteratorStream() {
        List<Integer> values = StreamSupport.stream(new Counting(), false)
            .collect(Collectors.toList());
        return values.size() + "|" + values.get(0) + "|" + values.get(2);
    }

    public static String charsets() {
        Charset utf8 = Objects.requireNonNull(StandardCharsets.UTF_8, "charset");
        String unsupported;
        try {
            Charset.forName("no-such-charset");
            unsupported = "found";
        } catch (IllegalArgumentException error) {
            unsupported = "unsupported";
        }
        return utf8.name() + "|" + Charset.forName("utf8").equals(utf8) + "|"
            + StandardCharsets.ISO_8859_1.name() + "|" + unsupported;
    }

    public static String loggers() {
        org.apache.logging.log4j.Logger log4j =
            org.apache.logging.log4j.LogManager.getLogger(Probe.class);
        org.slf4j.Logger slf4j = org.slf4j.LoggerFactory.getLogger("demo-config");
        log4j.info("value {}", "x");
        slf4j.warn("careful");
        return (log4j != null) + "|" + log4j.getName() + "|" + log4j.isDebugEnabled() + "|"
            + slf4j.getName() + "|" + slf4j.isInfoEnabled();
    }

    public static String emptyService() {
        ServiceLoader<Integration> loader = ServiceLoader.load(Integration.class);
        Iterator<Integration> providers = loader.iterator();
        return String.valueOf(providers.hasNext());
    }

    public static String resolveSibling() {
        return Path.of("/cfg/a.json").resolveSibling("b.json").toString();
    }

    public static String subList() {
        List<Integer> values = new ArrayList<>();
        values.add(1);
        values.add(2);
        values.add(3);
        values.add(4);
        List<Integer> middle = values.subList(1, 3);
        String reversed;
        try {
            values.subList(3, 1);
            reversed = "no-throw";
        } catch (IllegalArgumentException error) {
            reversed = "illegal";
        }
        return middle.size() + "|" + middle.get(0) + "|" + middle.get(1) + "|" + reversed;
    }

    public static String constructorReference() {
        Supplier<Map<String, Integer>> factory = HashMap::new;
        Map<String, Integer> map = factory.get();
        map.put("a", 1);
        map.put("b", 2);
        return (map != null) + "|" + map.size() + "|" + map.get("b");
    }

    public static String intStreams() {
        List<String> labels = IntStream.rangeClosed(1, 3).mapToObj(i -> "#" + i)
            .collect(Collectors.toList());
        int sum = IntStream.range(0, 5).sum();
        long count = IntStream.range(2, 2).count();
        return labels.toString() + "|" + sum + "|" + count;
    }

    public static String ordering() {
        List<String> words = List.of("pear", "fig", "apple", "kiwi");
        List<String> natural = words.stream().sorted().collect(Collectors.toList());
        List<String> byLength = words.stream()
            .sorted(Comparator.comparingInt(String::length).thenComparing(Comparator.reverseOrder()))
            .collect(Collectors.toList());
        String longest = words.stream().max(Comparator.comparingInt(String::length)).orElse("none");
        String shortest = words.stream().min(Comparator.comparing(String::length)).orElse("none");
        Optional<String> first = words.stream().findFirst();
        String none = List.<String>of().stream().max(Comparator.naturalOrder()).orElse("none");
        int total = List.of(1, 2, 3).stream().reduce(0, Integer::sum);
        String joined = words.stream().reduce((a, b) -> a + "+" + b).orElse("");
        Comparator<String> reversed = Comparator.<String>naturalOrder().reversed();
        return natural.toString() + "|" + byLength.toString() + "|" + longest + "|" + shortest + "|" + first.get()
            + "|" + none + "|" + total + "|" + joined + "|" + reversed.compare("a", "b");
    }

    public static String serviceRanking() {
        List<Ranked> services = List.of(new Low(), new Off(), new High());
        Ranked best = services.stream().filter(Ranked::active)
            .max(Comparator.comparingInt(Ranked::priority)).orElseThrow();
        return best.getClass().getSimpleName() + "|" + best.priority();
    }

    public static String streamIterator() {
        Iterator<String> values = List.of("x", "y").stream().map(s -> s + s).iterator();
        StringBuilder out = new StringBuilder();
        while (values.hasNext()) {
            out.append(values.next());
        }
        return out.toString();
    }

    public static String boxedConcat() {
        Boolean flag = Boolean.TRUE;
        Integer number = 42;
        Object anything = Long.valueOf(9);
        return "" + flag + "|" + number + "|" + anything + "|" + ("" + flag).length();
    }

    public static String gsonTypedWriter() {
        java.io.StringWriter writer = new java.io.StringWriter();
        new com.google.gson.Gson().toJson(List.of(1, 2), List.class, writer);
        return writer.toString();
    }

    public static String entryList() {
        Map<String, Integer> map = new HashMap<>();
        map.put("b,2", 2);
        map.put("a", 1);
        map.put("c", 3);
        List<Map.Entry<String, Integer>> entries = new ArrayList<>(map.entrySet());
        entries.sort(Map.Entry.comparingByKey());
        StringBuilder out = new StringBuilder();
        for (Map.Entry<String, Integer> entry : entries) {
            out.append(entry.getKey()).append('=').append(entry.getValue()).append(';');
        }
        List<Integer> numbers = new ArrayList<>(List.of(3, 1, 2));
        java.util.Collections.sort(numbers);
        numbers.sort(Comparator.reverseOrder());
        return out + "|" + numbers.get(0) + numbers.get(1) + numbers.get(2) + "|"
            + map.containsKey("b,2") + "|" + map.get("b,2");
    }

    public static String formats() {
        return String.format("[%04x|%-5s|%5s|%05d|%-4d|%X|%.2s|%x]", 255, "ab", "ab", -42, 7, 255,
            "hello", -1) + String.format("%.2f|%.2f|%.0f", 2.675, 9.995, 2.5);
    }

    public static String entrySetValue() {
        Map<String, List<Integer>> coats = new HashMap<>();
        coats.put("bay", new ArrayList<>(List.of(1, 2)));
        coats.put("grey", new ArrayList<>(List.of(3)));
        int replaced = 0;
        for (Map.Entry<String, List<Integer>> entry : coats.entrySet()) {
            List<Integer> before = entry.setValue(List.of(0, 0, 0, 0));
            replaced += before.size();
        }
        String immutable;
        try {
            Map.entry("k", 1).setValue(2);
            immutable = "set";
        } catch (UnsupportedOperationException error) {
            immutable = "unsupported";
        }
        return replaced + "|" + coats.get("bay").size() + "|" + coats.get("grey").size() + "|"
            + immutable;
    }

    public static String copies() {
        List<String> source = new ArrayList<>(List.of("a", "b", "a"));
        List<String> list = List.copyOf(source);
        java.util.Set<String> set = java.util.Set.copyOf(source);
        source.add("c");
        String nulls;
        try {
            List<String> withNull = new ArrayList<>();
            withNull.add(null);
            List.copyOf(withNull);
            nulls = "copied";
        } catch (NullPointerException error) {
            nulls = "npe";
        }
        return list.size() + "|" + list.get(1) + "|" + set.size() + "|" + nulls;
    }

    public static String classKinds() {
        return Marker.class.isInterface() + "|" + Probe.class.isInterface() + "|"
            + Ranked.class.isInterface();
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
            "horizon-native-java-jdk-{test}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let mut fixture = Self {
            root,
            jar: NativeJar::default(),
        };
        let classes = fixture.root.join("classes");
        let sources = [
            ("org/apache/logging/log4j/LogManager.java", LOG4J_MANAGER),
            ("org/apache/logging/log4j/Logger.java", LOG4J_LOGGER),
            ("org/slf4j/LoggerFactory.java", SLF4J_FACTORY),
            ("org/slf4j/Logger.java", SLF4J_LOGGER),
            ("com/google/gson/Gson.java", GSON),
            ("demo/Probe.java", PROBE),
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
        assert!(status.success(), "javac failed for jdk fixture {test}");

        let jar_path = fixture.root.join("jdk.jar");
        let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).unwrap());
        zip.start_file("fabric.mod.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            br#"{"id":"jdk-fixture","version":"1.0","entrypoints":{"main":"demo.Probe"}}"#,
        )
        .unwrap();
        // Only the mod's own classes ship: the logging and Gson stubs exist for
        // javac.
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
fn class_typed_calls_reach_interface_defaults_and_receiver_overrides() {
    // `TomlLikeParser.parse` is only an interface default; `BaseShape.name()`
    // is only declared (abstractly) by `Shape`, so the receiver picks it.
    let fixture = Fixture::build("defaults");
    assert_eq!(fixture.text("defaults"), "parsed:x|shape:square");
}

#[test]
fn a_jar_reader_subclass_runs_its_own_read() {
    // The call site is `java/io/Reader.read()`; the JVM selects the override.
    let fixture = Fixture::build("reader");
    assert_eq!(fixture.text("readerSubclass"), "abc");
}

#[test]
fn instanceof_sees_boxed_and_collection_supertypes() {
    let fixture = Fixture::build("instanceof");
    assert_eq!(
        fixture.text("instanceOf"),
        "true|true|false|true|false|true|true|false"
    );
}

#[test]
fn char_sequence_calls_answer_from_the_text() {
    let fixture = Fixture::build("charseq");
    assert_eq!(fixture.text("charSequence"), "5|e|2|el");
}

#[test]
fn collectors_to_map_keys_values_rejects_duplicates_and_merges() {
    let fixture = Fixture::build("tomap");
    assert_eq!(fixture.text("toMap"), "3|2|duplicate|2|1");
}

#[test]
fn stream_support_drains_a_jar_spliterator() {
    let fixture = Fixture::build("spliterator");
    assert_eq!(fixture.text("spliteratorStream"), "3|0|2");
}

#[test]
fn standard_charsets_are_real_values_and_unknown_names_throw() {
    let fixture = Fixture::build("charsets");
    assert_eq!(
        fixture.text("charsets"),
        "UTF-8|true|ISO-8859-1|unsupported"
    );
}

#[test]
fn logger_factories_answer_named_disabled_loggers() {
    let fixture = Fixture::build("loggers");
    assert_eq!(
        fixture.text("loggers"),
        "true|demo.Probe|false|demo-config|false"
    );
}

#[test]
fn a_mod_service_with_no_providers_is_an_empty_loader() {
    let fixture = Fixture::build("service");
    assert_eq!(fixture.text("emptyService"), "false");
}

#[test]
fn path_resolve_sibling_replaces_the_file_name() {
    let fixture = Fixture::build("sibling");
    assert_eq!(fixture.text("resolveSibling"), "/cfg/b.json");
}

#[test]
fn sub_list_copies_the_range_and_checks_bounds() {
    let fixture = Fixture::build("sublist");
    assert_eq!(fixture.text("subList"), "2|2|3|illegal");
}

#[test]
fn a_jdk_constructor_reference_builds_a_usable_collection() {
    let fixture = Fixture::build("ctor-ref");
    assert_eq!(fixture.text("constructorReference"), "true|2|2");
}

#[test]
fn int_streams_range_map_and_sum() {
    let fixture = Fixture::build("intstream");
    assert_eq!(fixture.text("intStreams"), "[#1, #2, #3]|10|0");
}

#[test]
fn sorted_max_min_reduce_and_comparator_combinators() {
    let fixture = Fixture::build("ordering");
    assert_eq!(
        fixture.text("ordering"),
        "[apple, fig, kiwi, pear]|[fig, pear, kiwi, apple]|apple|fig|pear|none|6|pear+fig+apple+kiwi|1"
    );
}

#[test]
fn interface_method_references_dispatch_on_the_provider() {
    // `Ranked::active` and `Ranked::priority` name the interface; each
    // provider class answers its own.
    let fixture = Fixture::build("ranking");
    assert_eq!(fixture.text("serviceRanking"), "High|7");
}

#[test]
fn a_stream_iterator_walks_its_elements() {
    let fixture = Fixture::build("stream-iterator");
    assert_eq!(fixture.text("streamIterator"), "xxyy");
}

#[test]
fn concatenating_a_box_prints_its_value() {
    let fixture = Fixture::build("boxed-concat");
    assert_eq!(fixture.text("boxedConcat"), "true|42|9|4");
}

#[test]
fn class_is_interface_reads_the_jar_class_flags() {
    let fixture = Fixture::build("class-kinds");
    assert_eq!(fixture.text("classKinds"), "true|false|true");
}

#[test]
fn a_nested_jar_service_file_seeds_its_providers() {
    // A JiJ library's own `META-INF/services` rows, as the pack class path
    // links them for Xaero's bundled `xaerolib`.
    let mut table = LinkedClassTable::default();
    table.link_service_file(
        "META-INF/services/demo.api.Platform",
        b"# the fabric helper\ndemo.fabric.FabricPlatform  \n\ndemo.Fallback # last\n",
    );
    table.link_service_file("META-INF/MANIFEST.MF", b"demo.Ignored\n");
    table.link_service_file("META-INF/services/nested/dir", b"demo.Ignored\n");
    assert_eq!(
        table.service_providers("demo/api/Platform"),
        ["demo/fabric/FabricPlatform", "demo/Fallback"]
    );
    assert_eq!(table.provider_count(), 2);
}

#[test]
fn gson_to_json_with_a_type_writes_to_the_last_argument() {
    // `toJson(src, typeOfSrc, writer)`: emotecraft saves its config this way.
    let fixture = Fixture::build("gson-typed");
    assert_eq!(fixture.text("gsonTypedWriter"), "[1,2]");
}

#[test]
fn entry_lists_round_trip_and_sort_like_the_jdk() {
    // `new ArrayList<>(map.entrySet())` then `sort(comparingByKey())`, the
    // idiom config writers (peek, sound-physics) use to order their keys; a
    // key holding the collection separator survives the copy and the lookup.
    let fixture = Fixture::build("entry-list");
    assert_eq!(fixture.text("entryList"), "a=1;b,2=2;c=3;|321|true|2");
}

#[test]
fn string_format_widths_flags_hex_and_half_up_precision() {
    // The JDK's `Formatter`: `%.2f` rounds the shortest digits half-up.
    let fixture = Fixture::build("formats");
    assert_eq!(
        fixture.text("formats"),
        "[00ff|ab   |   ab|-0042|7   |FF|he|ffffffff]2.68|10.00|3"
    );
}

#[test]
fn entry_set_value_writes_through_to_its_map() {
    let fixture = Fixture::build("entry-set-value");
    assert_eq!(fixture.text("entrySetValue"), "3|4|4|unsupported");
}

#[test]
fn collection_copy_of_copies_and_rejects_nulls() {
    let fixture = Fixture::build("copy-of");
    assert_eq!(fixture.text("copies"), "3|b|2|npe");
}
