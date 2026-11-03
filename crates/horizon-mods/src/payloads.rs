//! A bounded frame model for the payloads a native Java mod registers.
//!
//! The native host records a [`horizon_native_java::PayloadRegistration`] as
//! an opaque marker: its `payload_type` is a value, not an id. A loader
//! adapter resolves it to a namespaced [`PayloadDeclaration`]; this module
//! carries those declarations and the length-prefixed [`PayloadFrame`] once a
//! payload has bytes. Channel ownership, ordering and sending stay with the
//! game's transport.

use std::collections::BTreeMap;

/// A payload id is a resource location; a longer one is rejected.
pub const MAX_PAYLOAD_ID: usize = 256;
/// The payload bytes one frame may carry; a larger packet is refused, not
/// buffered.
pub const MAX_PAYLOAD_BYTES: usize = 1 << 20;
/// Declarations one registry holds.
pub const MAX_PAYLOAD_DECLARATIONS: usize = 4_096;
/// The largest `length` field: the fixed header, a maximal id and payload.
const MAX_FRAME_REST: usize = 3 + MAX_PAYLOAD_ID + MAX_PAYLOAD_BYTES;

/// The channels a `PayloadRegistrar` call names. A frame travels on one
/// concrete channel, so the play directions share
/// [`PayloadDirection::PlayBidirectional`] as a declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PayloadDirection {
    PlayToClient,
    PlayToServer,
    ConfigurationToClient,
    ConfigurationToServer,
    PlayBidirectional,
}

impl PayloadDirection {
    /// The name a native `PayloadRegistrar` call records (`playToServer`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::PlayToClient => "playToClient",
            Self::PlayToServer => "playToServer",
            Self::ConfigurationToClient => "configurationToClient",
            Self::ConfigurationToServer => "configurationToServer",
            Self::PlayBidirectional => "playBidirectional",
        }
    }

    /// Parse a native call name or the snake_case spelling the compatibility
    /// report prints. `configurationBidirectional` is deliberately absent:
    /// this model has no configuration frame that travels both ways, so it is
    /// an error instead of a guessed channel.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "playToClient" | "play_to_client" => Ok(Self::PlayToClient),
            "playToServer" | "play_to_server" => Ok(Self::PlayToServer),
            "configurationToClient" | "configuration_to_client" => Ok(Self::ConfigurationToClient),
            "configurationToServer" | "configuration_to_server" => Ok(Self::ConfigurationToServer),
            "playBidirectional" | "play_bidirectional" => Ok(Self::PlayBidirectional),
            "configurationBidirectional" | "configuration_bidirectional" => Err(
                "payload direction configurationBidirectional has no bounded frame channel"
                    .to_owned(),
            ),
            other => Err(format!("unknown payload direction {other:?}")),
        }
    }

    /// The direction byte a frame carries. The values are part of the wire
    /// format; explicit values mean reordering the enum cannot move a channel.
    pub const fn wire(self) -> u8 {
        match self {
            Self::PlayToClient => 0,
            Self::PlayToServer => 1,
            Self::ConfigurationToClient => 2,
            Self::ConfigurationToServer => 3,
            Self::PlayBidirectional => 4,
        }
    }

    pub fn from_wire(byte: u8) -> Result<Self, String> {
        match byte {
            0 => Ok(Self::PlayToClient),
            1 => Ok(Self::PlayToServer),
            2 => Ok(Self::ConfigurationToClient),
            3 => Ok(Self::ConfigurationToServer),
            4 => Ok(Self::PlayBidirectional),
            other => Err(format!("unknown payload frame direction {other}")),
        }
    }
}

/// One payload channel a mod declares: its namespaced id, direction and the
/// optional stream codec id the registration carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadDeclaration {
    pub id: String,
    pub direction: PayloadDirection,
    pub codec: Option<String>,
}

impl PayloadDeclaration {
    /// The id, and its codec when present, are resource locations (vanilla
    /// codec ids are legitimate).
    pub fn validate(&self) -> Result<(), String> {
        check_id(&self.id)?;
        if let Some(codec) = &self.codec {
            check_id(codec)?;
        }
        Ok(())
    }
}

/// The payload kinds the transport can check without running mod code. Any
/// other codec id is treated as opaque bytes: the frame still travels, but
/// only its declared id and direction are validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadCodec {
    /// The codec's bytes are opaque (`minecraft:byte_array`, `minecraft:raw`).
    Raw,
    /// A UTF-8 string (`minecraft:string`, `minecraft:utf8`).
    Utf8,
    /// No payload bytes (`minecraft:unit`, `minecraft:empty`).
    Empty,
}

impl PayloadCodec {
    /// The codec a declaration names, when it is one the transport checks.
    pub fn parse(codec: &str) -> Option<Self> {
        match codec {
            "minecraft:byte_array" | "minecraft:raw" | "minecraft:byte_buffer" => Some(Self::Raw),
            "minecraft:string" | "minecraft:utf8" | "minecraft:utf_8" => Some(Self::Utf8),
            "minecraft:unit" | "minecraft:empty" | "minecraft:void" => Some(Self::Empty),
            _ => None,
        }
    }

    /// Check a frame's bytes against this codec.
    pub fn validate(self, bytes: &[u8]) -> Result<(), String> {
        match self {
            Self::Raw => Ok(()),
            Self::Utf8 => std::str::from_utf8(bytes)
                .map(|_| ())
                .map_err(|_| "payload codec minecraft:string expects UTF-8 bytes".to_owned()),
            Self::Empty if !bytes.is_empty() => Err(format!(
                "payload codec minecraft:empty expects no bytes, got {}",
                bytes.len()
            )),
            Self::Empty => Ok(()),
        }
    }
}

/// Validate a frame against its declaration's codec, when the codec is one
/// the transport models. Unknown codecs stay opaque.
pub fn validate_payload(declaration: &PayloadDeclaration, bytes: &[u8]) -> Result<(), String> {
    match declaration.codec.as_deref().and_then(PayloadCodec::parse) {
        Some(codec) => codec.validate(bytes),
        None => Ok(()),
    }
}

/// A namespaced resource location, bounded and free of control characters so
/// a diagnostic can carry it into a log.
fn check_id(id: &str) -> Result<(), String> {
    let Some((namespace, path)) = id.split_once(':') else {
        return Err(format!("payload id {id:?} has no namespace"));
    };
    let short = |text: &str| {
        text.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
    };
    if id.len() <= MAX_PAYLOAD_ID
        && !namespace.is_empty()
        && !path.is_empty()
        && short(namespace)
        && path
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_./-".contains(&b))
    {
        return Ok(());
    }
    Err(format!("payload id {id:?} is not a resource location"))
}

/// The declarations one mod bootstrap produced, ordered by id.
///
/// One id names one channel, so the same declaration twice is idempotent and
/// any other repeated id is a conflict: a frame's id and direction must
/// identify exactly one declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PayloadRegistry {
    declarations: BTreeMap<String, PayloadDeclaration>,
}

impl PayloadRegistry {
    /// A duplicate is accepted only when identical; a different direction or
    /// codec for the same id is a conflict.
    pub fn register(&mut self, declaration: PayloadDeclaration) -> Result<(), String> {
        declaration.validate()?;
        if let Some(existing) = self.declarations.get(&declaration.id) {
            if existing == &declaration {
                return Ok(());
            }
            return Err(format!(
                "payload {:?} conflicts with its {} declaration (codec {:?})",
                declaration.id,
                existing.direction.name(),
                existing.codec
            ));
        }
        if self.declarations.len() >= MAX_PAYLOAD_DECLARATIONS {
            return Err(format!(
                "more than {MAX_PAYLOAD_DECLARATIONS} payload declarations"
            ));
        }
        self.declarations
            .insert(declaration.id.clone(), declaration);
        Ok(())
    }

    /// Every declaration, sorted by id.
    pub fn declarations(&self) -> impl Iterator<Item = &PayloadDeclaration> {
        self.declarations.values()
    }

    pub fn get(&self, id: &str) -> Option<&PayloadDeclaration> {
        self.declarations.get(id)
    }

    /// The declaration that owns `id` on `direction`. A
    /// [`PayloadDirection::PlayBidirectional`] declaration owns both play
    /// directions; a concrete one does not answer for its opposite.
    pub fn lookup(&self, id: &str, direction: PayloadDirection) -> Option<&PayloadDeclaration> {
        let declaration = self.declarations.get(id)?;
        let play = matches!(
            direction,
            PayloadDirection::PlayToClient | PayloadDirection::PlayToServer
        );
        let both = declaration.direction == PayloadDirection::PlayBidirectional && play;
        (declaration.direction == direction || both).then_some(declaration)
    }

    /// Build a registry from `(id, direction, codec)` rows, parsing the
    /// direction spelling a native registration carries.
    ///
    /// The native `PayloadRegistration` cannot be passed directly: its
    /// `payload_type` is an opaque value, so only the loader adapter that owns
    /// the Java object graph can resolve it to a resource location. That
    /// adapter hands this function the id it resolved; the frame model never
    /// guesses one from a marker.
    pub fn from_registrations<'a>(
        registrations: impl IntoIterator<Item = (&'a str, &'a str, Option<&'a str>)>,
    ) -> Result<Self, String> {
        let mut registry = Self::default();
        for (id, direction, codec) in registrations {
            let direction = PayloadDirection::parse(direction)
                .map_err(|error| format!("payload {id:?}: {error}"))?;
            registry.register(PayloadDeclaration {
                id: id.to_owned(),
                direction,
                codec: codec.map(str::to_owned),
            })?;
        }
        Ok(registry)
    }
}

/// One payload frame: its channel, its id and its bytes.
///
/// The wire format is big-endian: `u32 length` (everything after it), `u8
/// direction`, `u16 id length`, the UTF-8 id, then the payload. A frame id is
/// byte-faithful bounded text so a transport can carry what a codec produced;
/// the declarations a registry accepts are resource locations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadFrame {
    id: String,
    direction: PayloadDirection,
    bytes: Vec<u8>,
}

impl PayloadFrame {
    /// A frame whose id and payload are inside the model's bounds.
    pub fn new(id: &str, direction: PayloadDirection, bytes: Vec<u8>) -> Result<Self, String> {
        check_frame_id(id)?;
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(format!(
                "payload frame is {} bytes (max {MAX_PAYLOAD_BYTES})",
                bytes.len()
            ));
        }
        Ok(Self {
            id: id.to_owned(),
            direction,
            bytes,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn direction(&self) -> PayloadDirection {
        self.direction
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn encode(&self) -> Vec<u8> {
        let rest = 3 + self.id.len() + self.bytes.len();
        debug_assert!(rest <= MAX_FRAME_REST);
        let mut out = Vec::with_capacity(4 + rest);
        out.extend_from_slice(&(rest as u32).to_be_bytes());
        out.push(self.direction.wire());
        out.extend_from_slice(&(self.id.len() as u16).to_be_bytes());
        out.extend_from_slice(self.id.as_bytes());
        out.extend_from_slice(&self.bytes);
        out
    }

    /// Read exactly one frame from the front of `bytes`, returning it and how
    /// many bytes it used; anything after it is left for the next call.
    pub fn decode(bytes: &[u8]) -> Result<(Self, usize), String> {
        let header = bytes.get(..4).ok_or_else(|| {
            format!(
                "payload frame is {} bytes, shorter than its 4-byte header",
                bytes.len()
            )
        })?;
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if length > MAX_FRAME_REST {
            return Err(format!(
                "payload frame of {length} bytes exceeds {MAX_FRAME_REST}"
            ));
        }
        let end = 4usize
            .checked_add(length)
            .ok_or_else(|| format!("payload frame length {length} overflows"))?;
        if bytes.len() < end {
            return Err(format!(
                "payload frame is truncated: needs {end} bytes, has {}",
                bytes.len()
            ));
        }
        let (&wire, rest) = bytes[4..end]
            .split_first()
            .ok_or_else(|| "payload frame has no direction byte".to_owned())?;
        let direction = PayloadDirection::from_wire(wire)?;
        let (id_len, body) = rest
            .split_at_checked(2)
            .ok_or_else(|| "payload frame has no id length".to_owned())?;
        let id_len = u16::from_be_bytes([id_len[0], id_len[1]]) as usize;
        if id_len > MAX_PAYLOAD_ID {
            return Err(format!(
                "payload frame id is {id_len} bytes (max {MAX_PAYLOAD_ID})"
            ));
        }
        let (id, payload) = body
            .split_at_checked(id_len)
            .ok_or_else(|| format!("payload frame id is truncated: needs {id_len} bytes"))?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(format!(
                "payload frame carries {} bytes (max {MAX_PAYLOAD_BYTES})",
                payload.len()
            ));
        }
        let id = std::str::from_utf8(id)
            .map_err(|error| format!("payload frame id is not UTF-8: {error}"))?;
        check_frame_id(id)?;
        Ok((
            Self {
                id: id.to_owned(),
                direction,
                bytes: payload.to_vec(),
            },
            end,
        ))
    }
}

/// A frame id is plain bounded text: non-empty, no control characters. The
/// registry's namespaced check is stricter and applies to declarations.
fn check_frame_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("payload frame id is empty".to_owned());
    }
    if id.len() > MAX_PAYLOAD_ID {
        return Err(format!(
            "payload frame id is {} bytes (max {MAX_PAYLOAD_ID})",
            id.len()
        ));
    }
    if id.chars().any(char::is_control) {
        return Err(format!("payload frame id {id:?} has control characters"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use PayloadDirection::*;

    fn frame(id: &str, direction: PayloadDirection, bytes: &[u8]) -> PayloadFrame {
        PayloadFrame::new(id, direction, bytes.to_vec()).unwrap()
    }

    fn decl(id: &str, direction: &str, codec: Option<&str>) -> PayloadDeclaration {
        PayloadDeclaration {
            id: id.to_owned(),
            direction: PayloadDirection::parse(direction).unwrap(),
            codec: codec.map(str::to_owned),
        }
    }

    fn roundtrip(frame: PayloadFrame) {
        let encoded = frame.encode();
        assert_eq!(
            PayloadFrame::decode(&encoded).unwrap(),
            (frame, encoded.len())
        );
    }

    fn raw(direction: u8, id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = ((3 + id.len() + payload.len()) as u32)
            .to_be_bytes()
            .to_vec();
        out.push(direction);
        out.extend_from_slice(&(id.len() as u16).to_be_bytes());
        out.extend_from_slice(id);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn parses_directions() {
        for (name, direction) in [
            ("playToClient", PlayToClient),
            ("playToServer", PlayToServer),
            ("configurationToClient", ConfigurationToClient),
            ("configurationToServer", ConfigurationToServer),
            ("playBidirectional", PlayBidirectional),
        ] {
            assert_eq!(PayloadDirection::parse(name), Ok(direction));
            assert_eq!(PayloadDirection::from_wire(direction.wire()), Ok(direction));
        }
        // The compatibility report prints snake_case; both spellings parse.
        assert_eq!(PayloadDirection::parse("play_to_client"), Ok(PlayToClient));
        for bad in ["", "playToSideways", "configurationBidirectional"] {
            assert!(PayloadDirection::parse(bad).is_err(), "{bad}");
        }
        assert!(PayloadDirection::from_wire(5).is_err());
    }

    #[test]
    fn registry_orders_and_rejects_conflicts() {
        let mut registry = PayloadRegistry::default();
        for row in [
            ("demo:beta", "playToServer", None),
            ("demo:alpha", "playToClient", Some("demo:codec")),
            ("ruby:gamma", "playBidirectional", None),
        ] {
            registry.register(decl(row.0, row.1, row.2)).unwrap();
        }
        let ids: Vec<_> = registry.declarations().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["demo:alpha", "demo:beta", "ruby:gamma"]);

        // The same declaration again is the same registration.
        assert!(
            registry
                .register(decl("demo:alpha", "playToClient", Some("demo:codec")))
                .is_ok()
        );
        assert_eq!(registry.declarations().count(), 3);
        assert!(
            registry
                .register(decl("demo:alpha", "playToClient", None))
                .is_err()
        );
        assert!(
            registry
                .register(decl("demo:alpha", "playToServer", None))
                .is_err()
        );

        // A bidirectional declaration owns both play directions, no config one.
        for play in [PlayToClient, PlayToServer] {
            assert!(registry.lookup("ruby:gamma", play).is_some());
        }
        assert!(
            registry
                .lookup("ruby:gamma", ConfigurationToClient)
                .is_none()
        );
        assert!(registry.lookup("demo:alpha", PlayToServer).is_none());

        // Declarations are resource locations, codecs too.
        for bad in ["alpha", "Demo:alpha", "demo:", "demo:a b"] {
            assert!(
                registry.register(decl(bad, "playToClient", None)).is_err(),
                "{bad}"
            );
        }
        assert!(
            registry
                .register(decl("demo:bad", "playToClient", Some("not an id")))
                .is_err()
        );
    }

    #[test]
    fn adapts_registration_tuples() {
        let rows = [
            ("demo:ping", "playToServer", Some("demo:ping_codec")),
            ("demo:pong", "play_to_client", None),
        ];
        let registry = PayloadRegistry::from_registrations(rows).unwrap();
        assert_eq!(registry.declarations().count(), 2);
        assert_eq!(
            registry.get("demo:ping").unwrap().codec.as_deref(),
            Some("demo:ping_codec")
        );
        assert!(PayloadRegistry::from_registrations([("demo:x", "sideways", None)]).is_err());
        assert!(PayloadRegistry::from_registrations([("plain", "playToServer", None)]).is_err());
        let conflict = [
            ("demo:x", "playToServer", None),
            ("demo:x", "playToClient", None),
        ];
        assert!(PayloadRegistry::from_registrations(conflict).is_err());
    }

    #[test]
    fn frames_round_trip() {
        roundtrip(frame("demo:empty", PlayToClient, b""));
        roundtrip(frame("ruby:灯", ConfigurationToServer, b"multi-byte"));
        roundtrip(frame(
            "demo:deep/path",
            PlayBidirectional,
            &(0..=255).collect::<Vec<u8>>(),
        ));

        // Deterministic generated frames: ids, directions and payload lengths.
        let ids = ["a:b", "ruby:灯", "demo:deep_payload", "x:y.z"];
        let directions = [
            PlayToClient,
            PlayToServer,
            ConfigurationToClient,
            ConfigurationToServer,
            PlayBidirectional,
        ];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for step in 0..96usize {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let payload: Vec<u8> = (0..state as usize % 257)
                .map(|i| (state.rotate_left((i % 64) as u32) >> ((i % 8) * 8)) as u8)
                .collect();
            let direction = directions[step % directions.len()];
            let id = ids[(step + state as usize) % ids.len()];
            roundtrip(frame(id, direction, &payload));
        }
    }

    #[test]
    fn decodes_one_frame_and_rejects_truncation() {
        let first = frame("demo:first", PlayToServer, b"one");
        let second = frame("demo:second", PlayToClient, b"two");
        let mut stream = first.encode();
        stream.extend(second.encode());
        let (decoded, used) = PayloadFrame::decode(&stream).unwrap();
        assert_eq!(decoded, first);
        assert_eq!(PayloadFrame::decode(&stream[used..]).unwrap().0, second);
        for cut in 0..used {
            assert!(PayloadFrame::decode(&stream[..cut]).is_err(), "cut {cut}");
        }
    }

    #[test]
    fn rejects_oversized_frames() {
        // A declared length beyond the model's bound is refused before any
        // buffer is reserved.
        let mut declared = ((MAX_FRAME_REST + 1) as u32).to_be_bytes().to_vec();
        declared.extend_from_slice(&[0, 0, 1]);
        assert!(PayloadFrame::decode(&declared).is_err());
        assert!(PayloadFrame::decode(&raw(9, b"demo:x", b"")).is_err());
        assert!(PayloadFrame::decode(&raw(0, &vec![b'a'; MAX_PAYLOAD_ID + 1], b"")).is_err());
        assert!(PayloadFrame::decode(&raw(0, b"demo:x", &vec![0; MAX_PAYLOAD_BYTES + 1])).is_err());
        let long = "x".repeat(MAX_PAYLOAD_ID + 1);
        assert!(PayloadFrame::new(&long, PlayToClient, Vec::new()).is_err());
        let big = vec![0; MAX_PAYLOAD_BYTES + 1];
        assert!(PayloadFrame::new("demo:big", PlayToClient, big).is_err());
        assert!(PayloadFrame::new("", PlayToClient, Vec::new()).is_err());
    }

    #[test]
    fn supported_codecs_validate_their_bytes() {
        assert_eq!(
            PayloadCodec::parse("minecraft:string"),
            Some(PayloadCodec::Utf8)
        );
        assert_eq!(
            PayloadCodec::parse("minecraft:byte_array"),
            Some(PayloadCodec::Raw)
        );
        assert_eq!(
            PayloadCodec::parse("minecraft:unit"),
            Some(PayloadCodec::Empty)
        );
        assert_eq!(PayloadCodec::parse("demo:custom"), None);
        assert!(PayloadCodec::Utf8.validate("hé".as_bytes()).is_ok());
        assert!(PayloadCodec::Utf8.validate(&[0xff]).is_err());
        assert!(PayloadCodec::Empty.validate(&[]).is_ok());
        assert!(PayloadCodec::Empty.validate(&[1]).is_err());
        assert!(PayloadCodec::Raw.validate(&[0xff, 0x00]).is_ok());
        let declaration = PayloadDeclaration {
            id: "demo:string".into(),
            direction: PayloadDirection::PlayToServer,
            codec: Some("minecraft:string".into()),
        };
        assert!(validate_payload(&declaration, b"ok").is_ok());
        assert!(validate_payload(&declaration, &[0xff]).is_err());
    }
}
