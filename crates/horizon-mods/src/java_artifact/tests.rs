//! Sibling of `java_artifact.rs` (picked up by `lib.rs` as `java_artifact`'s
//! test module).
use super::*;

fn format() -> Format {
    Format::new(Abi {
        game: "26.3".into(),
        horizon_mod_abi: 7,
        native_hooks: None,
    })
}

fn classes() -> Vec<Source> {
    vec![
        Source::of_bytes("class", "a/B.class", b"\xCA\xFE\xBA\xBE one"),
        Source::of_bytes("class", "a/C.class", b"\xCA\xFE\xBA\xBE two"),
    ]
}

/// The same document Python's `json.dumps(sort_keys=True, separators=(",", ":"))`
/// writes, which is the shape a tool outside this crate recomputes.
#[test]
fn a_key_computed_elsewhere_is_the_key_here() {
    let entry = Provenance::new(
        "fixture",
        format(),
        [
            Source {
                kind: "class".into(),
                path: "a/B.class".into(),
                sha256: "00".repeat(32),
            },
            Source {
                kind: "class".into(),
                path: "a/C.class".into(),
                sha256: "01".repeat(32),
            },
        ],
        "check.json",
    );
    let python = concat!(
        r#"{"format":{"abi":{"game":"26.3","horizon_mod_abi":7},"ir_version":1},"inputs":["#,
        r#"{"kind":"class","path":"a/B.class","sha256":""#
    )
    .to_string()
        + &"00".repeat(32)
        + r#""},{"kind":"class","path":"a/C.class","sha256":""#
        + &"01".repeat(32)
        + r#""}],"output":"check.json","report":"fixture"}"#;
    assert_eq!(entry.canonical(), python);
    assert_eq!(
        entry.derive(),
        "f9a575e1ab92d8e65036c2058eb9e5295a93dfb374de816e888b490bc0796bab"
    );
}

#[test]
fn the_native_hook_level_is_part_of_the_key_only_when_declared() {
    let mut abi = format();
    let without = Provenance::new("woven", abi.clone(), [], "woven.jar");
    abi.abi.native_hooks = Some(30);
    let with = Provenance::new("woven", abi, [], "woven.jar");
    assert_ne!(without.key, with.key);
    assert!(with.canonical().contains(r#""native_hooks":30"#));
    // Absent, not `null`: a record written before hook levels existed reads as
    // one that declares none.
    assert!(!without.canonical().contains("native_hooks"));
}

/// Non-ASCII escapes as `\uXXXX` with lower-case hex, as Python writes them,
/// and reads back to the same string in `serde_json`.
#[test]
fn a_non_ascii_output_name_escapes_the_way_python_does() {
    let entry = Provenance::new("fixture", format(), [], "woven-café.json");
    assert!(
        entry
            .canonical()
            .ends_with(r#""output":"woven-caf\u00e9.json","report":"fixture"}"#)
    );
    let read: Provenance = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
    assert_eq!(read.output, "woven-café.json");
    assert_eq!(read.key, entry.key);
}

/// The example `docs/java-mod-distribution.md` quotes, kept equal to the page:
/// a one-class artifact of `ca fe ba be` against 26.3 / ABI 7 / IR 1.
#[test]
fn the_documented_example_is_the_key_the_docs_quote() {
    let entry = Provenance::new(
        "check",
        format(),
        [Source::of_bytes(
            "class",
            "example/Mod.class",
            b"\xca\xfe\xba\xbe",
        )],
        "check.json",
    );
    assert_eq!(
        entry.key,
        "e3e8966d1a503288c822e3c2f082b1760744391b33039bf649c8dc1c7ce1680b"
    );
    assert!(entry.canonical().contains(
        r#""sha256":"65ab12a8ff3263fbc257e5ddf0aa563c64573d0bab1f1115b9b107834cfa6971""#
    ));
    // And the page's worked example is this crate's own documented example, so
    // `cargo test --doc -p horizon-mods` runs it too.
    let docs = include_str!("../../../../docs/java-mod-distribution.md");
    assert!(docs.contains(&entry.key));
    assert!(docs.contains(&entry.canonical()));
}

#[test]
fn input_order_does_not_change_the_key() {
    let one = Provenance::new("check", format(), classes(), "check.json");
    let mut reversed = classes();
    reversed.reverse();
    let two = Provenance::new("check", format(), reversed, "check.json");
    assert_eq!(one.key, two.key);
    // Nor does listing the same bytes twice.
    let mut doubled = classes();
    doubled.extend(classes());
    let three = Provenance::new("check", format(), doubled, "check.json");
    assert_eq!(one.key, three.key);
    assert_eq!(one.inputs.len(), 2);
}

#[test]
fn swapped_class_bytes_are_a_different_artifact() {
    let names = [
        Source::of_bytes("class", "a/B.class", b"one"),
        Source::of_bytes("class", "a/C.class", b"two"),
    ];
    let swapped = [
        Source::of_bytes("class", "a/B.class", b"two"),
        Source::of_bytes("class", "a/C.class", b"one"),
    ];
    let a = Provenance::new("woven", format(), names, "woven.jar");
    let b = Provenance::new("woven", format(), swapped, "woven.jar");
    assert_ne!(a.key, b.key, "the path is hashed with the bytes");
}

#[test]
fn re_zipping_identical_classes_keeps_the_key() {
    // The artifact is keyed on the class bytes, not the jar that carried them,
    // so a re-zipped or re-signed jar of the same classes rebuilds nothing.
    let from_jar_a = Provenance::new("check", format(), classes(), "check.json");
    let from_jar_b = Provenance::new("check", format(), classes(), "check.json");
    assert_eq!(from_jar_a.key, from_jar_b.key);
    // A jar that is itself the input is keyed on its own bytes, which do change.
    let jar = Provenance::new(
        "check",
        format(),
        [Source::of_bytes(
            "jar",
            "mods/thing.jar",
            b"zip container A",
        )],
        "check.json",
    );
    let rezipped = Provenance::new(
        "check",
        format(),
        [Source::of_bytes("jar", "mods/thing.jar", b"compile")],
        "check.json",
    );
    assert_ne!(jar.key, rezipped.key);
}

#[test]
fn a_missing_input_keys_apart_from_its_absence_being_unrecorded() {
    let checked_absent = Provenance::new(
        "facade",
        format(),
        [Source::absent("class", "net/minecraft/X.class")],
        "facade.jar",
    );
    let empty = Provenance::new("facade", format(), [], "facade.jar");
    assert_ne!(checked_absent.key, empty.key);
    assert_eq!(checked_absent.inputs[0].path, "~net/minecraft/X.class");
}

#[test]
fn an_unchanged_artifact_is_fresh() {
    let entry = Provenance::new("check", format(), classes(), "check.json");
    assert_eq!(entry.check(format(), &classes()), Status::Fresh);
    let status = entry.check(format(), &classes());
    assert!(status.is_fresh());
    assert_eq!(status.explain(), "reusable");
    assert!(status.reasons().is_empty());
}

#[test]
fn each_version_names_its_own_rebuild() {
    let entry = Provenance::new("check", format(), classes(), "check.json");

    let mut abi = format();
    abi.ir_version += 1;
    assert_eq!(
        entry.check(abi, &classes()).reasons(),
        ["IR format 1 -> 2 (this program answers differently)"]
    );

    let mut abi = format();
    abi.abi.game = "26.4".into();
    assert_eq!(
        entry.check(abi, &classes()).reasons(),
        ["Minecraft 26.3 -> 26.4 (names and shapes regenerated)"]
    );

    let mut abi = format();
    abi.abi.horizon_mod_abi = 8;
    assert_eq!(
        entry.check(abi, &classes()).reasons(),
        ["Horizon mod ABI 7 -> 8"]
    );

    let mut abi = format();
    abi.abi.native_hooks = Some(30);
    assert_eq!(
        entry.check(abi, &classes()).reasons(),
        ["native hooks none -> 30"]
    );
}

#[test]
fn a_ui_only_release_does_not_rebuild() {
    // Same IR, same game, same ABI: a release that changes none of them.
    let entry = Provenance::new("check", format(), classes(), "check.json");
    let mut later = format();
    later.abi.native_hooks = None;
    assert!(entry.check(later, &classes()).is_fresh());
}

#[test]
fn an_edited_class_or_a_new_one_is_stale_by_name() {
    let entry = Provenance::new("check", format(), classes(), "check.json");
    let mut edited = classes();
    edited[1] = Source::of_bytes("class", "a/C.class", b"\xCA\xFE\xBA\xBE two, smarter");
    assert_eq!(
        entry.check(format(), &edited).reasons(),
        ["class a/C.class changed"]
    );

    let mut grown = classes();
    grown.push(Source::of_bytes("class", "a/D.class", b"three"));
    assert_eq!(
        entry.check(format(), &grown).reasons(),
        ["class a/D.class appeared"]
    );

    assert_eq!(
        entry.check(format(), &classes()[..1]).reasons(),
        ["class a/C.class left"]
    );
}

#[test]
fn a_tampered_record_is_corrupt_not_stale() {
    let mut entry = Provenance::new("check", format(), classes(), "check.json");
    entry.output = "other.json".into();
    assert_eq!(entry.check(format(), &classes()), Status::Corrupt);
    assert_eq!(
        entry.check(format(), &classes()).reasons(),
        ["recorded key does not match its own contents"]
    );

    let mut entry = Provenance::new("check", format(), classes(), "check.json");
    entry.format.abi.horizon_mod_abi = 6;
    assert_eq!(entry.check(format(), &classes()), Status::Corrupt);
}

#[test]
fn a_record_survives_a_json_round_trip_and_still_checks() {
    let entry = Provenance::new("woven", format(), classes(), "woven.jar");
    let json = serde_json::to_string_pretty(&entry).unwrap();
    let read: Provenance = serde_json::from_str(&json).unwrap();
    assert_eq!(read, entry);
    assert_eq!(read.check(format(), &classes()), Status::Fresh);
    // Unknown fields are refused rather than silently dropped: a record from
    // a future format must not be read as this one.
    let unknown = json.trim_end_matches('}').to_owned() + ",\"future\":1}";
    assert!(serde_json::from_str::<Provenance>(&unknown).is_err());
}

#[test]
fn a_file_source_hashes_what_it_reads() {
    let dir = std::env::temp_dir().join(format!("horizon-java-artifact-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("A.class");
    fs::write(&path, b"class bytes").unwrap();
    let source = Source::of_file("class", &path).unwrap();
    assert_eq!(source.sha256, sha256_hex(b"class bytes"));
    assert!(source.path.ends_with("A.class"));
    assert!(Source::of_file("class", &dir.join("missing.class")).is_err());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_key_is_sixty_four_lower_case_hex_digits() {
    let key = Provenance::new("check", format(), classes(), "check.json").key;
    assert_eq!(key.len(), 64);
    assert!(
        key.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}

#[test]
fn a_record_prints_what_it_is() {
    let entry = Provenance::new("check", format(), classes(), "check.json");
    assert_eq!(
        entry.to_string(),
        "check [check.json] from 2 input(s), Minecraft 26.3, Horizon mod ABI 7, IR 1"
    );
}
