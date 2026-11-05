//! Bounded transport for mod payload channels (`horizon-mods::payloads`).
//!
//! A mod declares channels during bootstrap; the server sends the declaration
//! list on `horizon:payload_channels`, and both sides exchange frames on
//! `horizon:payload`. A frame is only accepted when its id is declared, the
//! direction matches, and a supported codec validates its bytes. Transport is
//! deliberately separate from the packet loop so both the hub and the remote
//! client can drive the same state machine.

use horizon_mods::payloads::{
    PayloadDeclaration, PayloadDirection, PayloadFrame, PayloadRegistry, PayloadValue, marker_hex,
    validate_payload,
};
use horizon_native_java::{
    NativeJar, PayloadRegistration, StdHost, Value, dispatch_payload_frame_with,
};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// The play channel frames travel on.
pub const CHANNEL: &str = "horizon:payload";
/// The channel the server announces its declarations on.
pub const REGISTER_CHANNEL: &str = "horizon:payload_channels";

const MAX_INCOMING: usize = 256;
const MAX_OUTGOING: usize = 256;
const MAX_REGISTRATION: usize = 64 * 1024;
const MAX_DECLARATIONS: usize = 4096;

/// One connection's payload state: the declarations both sides agreed on,
/// received frames waiting for the game, and encoded frames waiting to be
/// sent.
#[derive(Clone, Debug, Default)]
pub struct PayloadLink {
    registry: PayloadRegistry,
    incoming: Vec<PayloadFrame>,
    outgoing: Vec<Vec<u8>>,
}

impl PayloadLink {
    pub fn new(registry: PayloadRegistry) -> Self {
        Self {
            registry,
            incoming: Vec::new(),
            outgoing: Vec::new(),
        }
    }

    /// A link over the declarations the last content root recorded.
    pub fn from_declared() -> Self {
        Self::new(declared())
    }

    pub fn registry(&self) -> &PayloadRegistry {
        &self.registry
    }

    /// The declarations as they travel to the other side. `u16` count, then
    /// per declaration: `u16` id length, id, direction byte, `u8` has-codec,
    /// and (with codec) `u16` length and text. Bounded to
    /// [`MAX_REGISTRATION`].
    pub fn registration(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // Reserve the count until we know how many complete declarations fit.
        // Truncating a finished buffer can cut a length-prefixed id or codec
        // in half, making an otherwise valid registry impossible to apply.
        out.extend_from_slice(&0u16.to_be_bytes());
        let mut count = 0u16;
        for declaration in self.registry.declarations().take(u16::MAX as usize) {
            let mut encoded = Vec::new();
            encoded.extend_from_slice(&(declaration.id.len() as u16).to_be_bytes());
            encoded.extend_from_slice(declaration.id.as_bytes());
            encoded.push(declaration.direction.wire());
            match &declaration.codec {
                Some(codec) => {
                    encoded.push(1);
                    encoded.extend_from_slice(&(codec.len() as u16).to_be_bytes());
                    encoded.extend_from_slice(codec.as_bytes());
                }
                None => encoded.push(0),
            }
            if out.len() + encoded.len() > MAX_REGISTRATION {
                break;
            }
            out.extend_from_slice(&encoded);
            count += 1;
        }
        out[..2].copy_from_slice(&count.to_be_bytes());
        out
    }

    /// Apply the other side's declaration list; returns how many channels it
    /// registered. Replaces the local registry.
    pub fn apply_registration(&mut self, data: &[u8]) -> Result<usize, String> {
        if data.len() > MAX_REGISTRATION {
            return Err(format!("payload registration is {} bytes", data.len()));
        }
        let mut reader = Reader { bytes: data, at: 0 };
        let count = reader.u16()? as usize;
        if count > MAX_DECLARATIONS {
            return Err(format!("payload registration lists {count} channels"));
        }
        let mut registry = PayloadRegistry::default();
        for _ in 0..count {
            let id = reader.text()?;
            let direction = PayloadDirection::from_wire(reader.u8()?)?;
            let codec = match reader.u8()? {
                0 => None,
                1 => Some(reader.text()?),
                other => return Err(format!("payload codec flag {other} is not 0 or 1")),
            };
            registry.register(PayloadDeclaration {
                id,
                direction,
                codec,
            })?;
        }
        if reader.at != data.len() {
            return Err("trailing bytes after payload registration".to_owned());
        }
        self.registry = registry;
        Ok(count)
    }

    /// Decode one frame from the other side, accepting it only when its id is
    /// declared, `expected` matches its direction (or a bidirectional
    /// declaration), and its codec validates the bytes.
    pub fn receive(
        &mut self,
        data: &[u8],
        expected: PayloadDirection,
    ) -> Result<&PayloadFrame, String> {
        let (frame, used) = PayloadFrame::decode(data)?;
        if used != data.len() {
            return Err(format!(
                "payload packet carries {} trailing bytes",
                data.len() - used
            ));
        }
        let declaration = self
            .registry
            .get(frame.id())
            .ok_or_else(|| format!("payload channel {} is not declared", frame.id()))?;
        if !accepts(declaration.direction, frame.direction()) || frame.direction() != expected {
            return Err(format!(
                "payload {} travels {} but was received as {}",
                frame.id(),
                frame.direction().name(),
                expected.name()
            ));
        }
        validate_payload(declaration, frame.bytes())?;
        if self.incoming.len() >= MAX_INCOMING {
            return Err("payload receive queue is full".to_owned());
        }
        self.incoming.push(frame);
        Ok(self.incoming.last().expect("just pushed"))
    }

    /// Encode a frame for the other side, validating the declaration, the
    /// direction and the supported codec first. The encoded bytes are also
    /// queued for [`take_outgoing`](Self::take_outgoing).
    pub fn send(
        &mut self,
        id: &str,
        direction: PayloadDirection,
        bytes: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        let declaration = self
            .registry
            .get(id)
            .ok_or_else(|| format!("payload channel {id} is not declared"))?;
        if !accepts(declaration.direction, direction) {
            return Err(format!(
                "payload {id} is declared {} not {}",
                declaration.direction.name(),
                direction.name()
            ));
        }
        validate_payload(declaration, &bytes)?;
        let encoded = PayloadFrame::new(id, direction, bytes)?.encode();
        if self.outgoing.len() >= MAX_OUTGOING {
            return Err("payload send queue is full".to_owned());
        }
        self.outgoing.push(encoded.clone());
        Ok(encoded)
    }

    pub fn take_incoming(&mut self) -> Vec<PayloadFrame> {
        std::mem::take(&mut self.incoming)
    }

    pub fn take_outgoing(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outgoing)
    }

    /// Take at most `max` received frames, oldest first, and keep the rest
    /// queued: a per-tick drain bounds its work without losing frames.
    pub fn take_incoming_up_to(&mut self, max: usize) -> Vec<PayloadFrame> {
        let count = max.min(self.incoming.len());
        self.incoming.drain(..count).collect()
    }

    pub fn incoming(&self) -> &[PayloadFrame] {
        &self.incoming
    }

    pub fn outgoing(&self) -> &[Vec<u8>] {
        &self.outgoing
    }
}

/// A declaration accepts a concrete direction when it names it or is the
/// bidirectional play form.
pub fn accepts(declared: PayloadDirection, direction: PayloadDirection) -> bool {
    declared == direction
        || (declared == PayloadDirection::PlayBidirectional
            && matches!(
                direction,
                PayloadDirection::PlayToClient | PayloadDirection::PlayToServer
            ))
}

/// The jars and registrations of running Java mods, so a received frame can
/// reach its registered handler.
struct HandlerJar {
    jar: NativeJar,
    registrations: Vec<PayloadRegistration>,
    /// The host the jar bootstrapped on, when the caller kept it. Its
    /// payload-type markers are what let a registration's marker resolve to
    /// a channel id; a handler jar stored without one never matches a frame.
    host: Option<StdHost>,
}

static HANDLER_JARS: Mutex<Vec<HandlerJar>> = Mutex::new(Vec::new());

/// Remember one mod jar's payload registrations; called after its bootstrap.
pub fn store_handler_jar(jar: NativeJar, registrations: Vec<PayloadRegistration>) {
    if registrations.is_empty() {
        return;
    }
    HANDLER_JARS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(HandlerJar {
            jar,
            registrations,
            host: None,
        });
}

/// [`store_handler_jar`] that also keeps the bootstrapped host, so a received
/// frame's channel id resolves to the registration's payload-type marker.
/// Registrations come from the host itself. Sends the host had queued are
/// dropped from the kept copy (the caller already published them).
pub fn store_handler_host(jar: NativeJar, host: &StdHost) {
    let registrations = host.payload_registrations().to_vec();
    if registrations.is_empty() {
        return;
    }
    let mut template = host.clone();
    let _ = template.take_outgoing_payloads();
    let _ = template.take_incoming_payloads();
    HANDLER_JARS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(HandlerJar {
            jar,
            registrations,
            host: Some(template),
        });
}

/// Forget the jars of a previous bootstrap.
pub fn clear_handler_jars() {
    HANDLER_JARS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Dispatch one received frame to the first registered handler that names its
/// channel. Returns whether a handler ran; sends it queues are published for
/// the next server tick.
pub fn dispatch_received(frame: &PayloadFrame) -> Result<bool, String> {
    dispatch_matching(frame, None)
}

/// [`dispatch_received`] for the side that received the frame: only
/// registrations that receive in `receiving` (their direction, or a
/// bidirectional one) can handle it. A client window passes `PlayToClient`,
/// so a server-side receiver of the same channel never answers its frames.
pub fn dispatch_received_as(
    frame: &PayloadFrame,
    receiving: PayloadDirection,
) -> Result<bool, String> {
    dispatch_matching(frame, Some(receiving))
}

/// Whether a registration's recorded direction receives frames travelling in
/// `receiving`. An unparseable direction (configuration phase) never does.
fn registration_receives(direction: &str, receiving: PayloadDirection) -> bool {
    match PayloadDirection::parse(direction) {
        Ok(PayloadDirection::PlayBidirectional) => true,
        Ok(direction) => direction == receiving,
        Err(_) => false,
    }
}

fn dispatch_matching(
    frame: &PayloadFrame,
    receiving: Option<PayloadDirection>,
) -> Result<bool, String> {
    let declarations = declared();
    let declaration = declarations.get(frame.id());
    let (payload, context) = payload_values(declaration, frame);
    let handlers = HANDLER_JARS.lock().unwrap_or_else(|e| e.into_inner());
    for handler in handlers.iter() {
        let registrations: Vec<PayloadRegistration> = handler
            .registrations
            .iter()
            .filter(|registration| {
                receiving.is_none_or(|direction| {
                    registration_receives(&registration.direction, direction)
                })
            })
            .cloned()
            .collect();
        let mut host = handler.host.clone().unwrap_or_else(StdHost::new);
        let matched = dispatch_payload_frame_with(
            &handler.jar,
            &mut host,
            &registrations,
            frame.id(),
            payload.clone(),
            context.clone(),
        )
        .map_err(|error| error.to_string())?;
        if matched {
            publish_from_host(&mut host)?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// The payload and context a handler receives. A supported codec gives it the
/// decoded string, an empty marker or the bounded bytes marker; a codec the
/// transport does not model stays the bounded `payload/<id>/<hex>` marker.
fn payload_values(
    declaration: Option<&PayloadDeclaration>,
    frame: &PayloadFrame,
) -> (Value, Value) {
    let id = frame.id();
    let context = Value::Ref(Some(format!("payload-context/{id}")));
    let codec = declaration.and_then(|declaration| declaration.codec.as_deref());
    let value = PayloadValue::decode(codec, frame.bytes())
        .unwrap_or_else(|_| PayloadValue::Marker(marker_hex(frame.bytes())));
    (payload_value(&value, id), context)
}

/// The native host has no byte-array value, so a raw payload crosses as its
/// bounded hex. `payload-bytes/<hex>` keeps it distinct from the
/// `payload/<id>/<hex>` marker of an unmodelled codec; both spell at most
/// [`MAX_MARKER_BYTES`](horizon_mods::payloads::MAX_MARKER_BYTES) bytes.
fn payload_value(value: &PayloadValue, id: &str) -> Value {
    match value {
        PayloadValue::Text(text) => Value::Ref(Some(text.clone())),
        PayloadValue::Empty => Value::Ref(Some(format!("payload-empty/{id}"))),
        PayloadValue::Bytes(bytes) => {
            Value::Ref(Some(format!("payload-bytes/{}", marker_hex(bytes))))
        }
        PayloadValue::Marker(hex) => Value::Ref(Some(format!("payload/{id}/{hex}"))),
    }
}

/// Frames a Java host asked to send, waiting for the server's next tick.
static JAVA_OUTGOING: Mutex<Vec<PayloadFrame>> = Mutex::new(Vec::new());

/// The `PlayToServer` frames of [`JAVA_OUTGOING`], mirrored for a client
/// window. The hub drains (and ignores) every frame of the shared queue each
/// tick, so a window cannot read its sends from there without racing it in an
/// integrated game; this copy is only ever drained by
/// [`take_java_to_server`]. A process that never drains it (a dedicated
/// server) drops the oldest frames instead of failing sends.
static JAVA_TO_SERVER: Mutex<Vec<PayloadFrame>> = Mutex::new(Vec::new());

/// Translate a Java host's queued sends into validated frames. A channel must
/// be declared, the direction must match, and a supported codec must accept
/// the bytes; the whole batch stops at the first bad frame.
pub fn publish_from_host_with(
    registry: &PayloadRegistry,
    host: &mut horizon_native_java::StdHost,
) -> Result<Vec<PayloadFrame>, String> {
    let mut frames = Vec::new();
    for (id, direction, bytes) in host.take_outgoing_payloads() {
        let direction = PayloadDirection::parse(&direction)?;
        let declaration = registry
            .get(&id)
            .ok_or_else(|| format!("payload channel {id} is not declared"))?;
        if !accepts(declaration.direction, direction) {
            return Err(format!(
                "payload {id} is declared {} not {}",
                declaration.direction.name(),
                direction.name()
            ));
        }
        validate_payload(declaration, &bytes)?;
        frames.push(PayloadFrame::new(&id, direction, bytes)?);
    }
    Ok(frames)
}

/// [`publish_from_host_with`] against the last stored declarations, queueing
/// the frames for the server's next tick. Returns how many were published.
pub fn publish_from_host(host: &mut horizon_native_java::StdHost) -> Result<usize, String> {
    let frames = publish_from_host_with(&declared(), host)?;
    let count = frames.len();
    if !frames.is_empty() {
        let mut queue = JAVA_OUTGOING.lock().unwrap_or_else(|e| e.into_inner());
        if queue.len() + frames.len() > MAX_OUTGOING {
            return Err("Java payload send queue is full".to_owned());
        }
        queue.extend(frames.iter().cloned());
        drop(queue);
        let mut mirror = JAVA_TO_SERVER.lock().unwrap_or_else(|e| e.into_inner());
        mirror.extend(
            frames
                .into_iter()
                .filter(|frame| frame.direction() == PayloadDirection::PlayToServer),
        );
        if mirror.len() > MAX_OUTGOING {
            let excess = mirror.len() - MAX_OUTGOING;
            mirror.drain(..excess);
        }
    }
    Ok(count)
}

/// Frames Java mods asked to send, drained in order.
pub fn take_java_outgoing() -> Vec<PayloadFrame> {
    std::mem::take(&mut JAVA_OUTGOING.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Take at most `max` frames Java mods addressed to the server, oldest first;
/// the rest stay queued for the next call. Each frame is returned once.
pub fn take_java_to_server(max: usize) -> Vec<PayloadFrame> {
    let mut queue = JAVA_TO_SERVER.lock().unwrap_or_else(|e| e.into_inner());
    let count = max.min(queue.len());
    queue.drain(..count).collect()
}

/// Take at most `max` frames Java mods addressed to a client (`PlayToClient`,
/// or the bidirectional play form), oldest first; the rest stay queued for
/// the next call. A server-bound frame is not this queue's to deliver: the
/// client window reads it from [`JAVA_TO_SERVER`] (`take_java_to_server`), so
/// it is never returned here and never broadcast by the hub.
pub fn take_java_to_clients(max: usize) -> Vec<PayloadFrame> {
    let mut queue = JAVA_OUTGOING.lock().unwrap_or_else(|e| e.into_inner());
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(queue.len());
    for frame in queue.drain(..) {
        let client_bound = matches!(
            frame.direction(),
            PayloadDirection::PlayToClient | PayloadDirection::PlayBidirectional
        );
        if !client_bound {
            // Its copy is already in the mirror for the client window.
            continue;
        }
        if taken.len() < max {
            taken.push(frame);
        } else {
            kept.push(frame);
        }
    }
    *queue = kept;
    taken
}

/// Hand received frames to a Java host for its bootstrap/bootstrap-time view.
/// Returns how many were delivered.
pub fn deliver_to_host(
    host: &mut horizon_native_java::StdHost,
    frames: impl IntoIterator<Item = PayloadFrame>,
) -> usize {
    let mut count = 0;
    for frame in frames {
        host.deliver_payload(frame.id(), frame.bytes().to_vec());
        count += 1;
    }
    count
}

/// The declarations of the last content root, for links built later.
static DECLARED: Mutex<Option<PayloadRegistry>> = Mutex::new(None);

/// Record the channels a content root declared. A repeated id with a
/// different direction or codec is a conflict, as at bootstrap.
pub fn store_declared(
    declarations: impl IntoIterator<Item = PayloadDeclaration>,
) -> Result<usize, String> {
    let mut registry = PayloadRegistry::default();
    let mut count = 0;
    for declaration in declarations {
        registry.register(declaration)?;
        count += 1;
    }
    *DECLARED.lock().unwrap_or_else(|e| e.into_inner()) = Some(registry);
    // Frames queued by a previous content root must not be sent using the
    // replacement registry. Drop them atomically with the declaration swap.
    JAVA_OUTGOING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    JAVA_TO_SERVER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    Ok(count)
}

/// The last stored declarations; empty until a content root records some.
pub fn declared() -> PayloadRegistry {
    DECLARED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// The declared channels keyed by id, for diagnostics.
pub fn declared_by_id() -> BTreeMap<String, PayloadDeclaration> {
    declared()
        .declarations()
        .map(|declaration| (declaration.id.clone(), declaration.clone()))
        .collect()
}

/// A tiny big-endian reader for the registration list.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, String> {
        let byte = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| "payload registration is truncated".to_owned())?;
        self.at += 1;
        Ok(byte)
    }

    fn u16(&mut self) -> Result<u16, String> {
        let hi = self.u8()?;
        let lo = self.u8()?;
        Ok(u16::from_be_bytes([hi, lo]))
    }

    fn text(&mut self) -> Result<String, String> {
        let len = self.u16()? as usize;
        let end = self
            .at
            .checked_add(len)
            .ok_or_else(|| "payload registration length overflows".to_owned())?;
        let text = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| "payload registration is truncated".to_owned())?;
        self.at = end;
        String::from_utf8(text.to_vec())
            .map_err(|_| "payload registration text is not UTF-8".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handler-jar store, declarations and Java queues are process-wide,
    /// so tests that touch them run one at a time.
    fn global_state() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn declaration(
        id: &str,
        direction: PayloadDirection,
        codec: Option<&str>,
    ) -> PayloadDeclaration {
        PayloadDeclaration {
            id: id.into(),
            direction,
            codec: codec.map(str::to_owned),
        }
    }

    fn registry(declarations: impl IntoIterator<Item = PayloadDeclaration>) -> PayloadRegistry {
        let mut registry = PayloadRegistry::default();
        for declaration in declarations {
            registry.register(declaration).unwrap();
        }
        registry
    }

    fn link() -> PayloadLink {
        PayloadLink::new(registry([declaration(
            "demo:to_server",
            PayloadDirection::PlayToServer,
            Some("minecraft:string"),
        )]))
    }

    #[test]
    fn registration_round_trips_and_replaces_the_registry() {
        let source = PayloadLink::new(registry([
            declaration(
                "demo:a",
                PayloadDirection::PlayToServer,
                Some("minecraft:raw"),
            ),
            declaration("demo:b", PayloadDirection::PlayToClient, None),
        ]));
        let mut target = PayloadLink::default();
        assert_eq!(
            target.apply_registration(&source.registration()).unwrap(),
            2
        );
        assert_eq!(target.registry().declarations().count(), 2);
        assert!(target.apply_registration(&[0xff]).is_err());
    }

    #[test]
    fn registration_cap_keeps_only_complete_declarations() {
        let mut source_registry = PayloadRegistry::default();
        for index in 0..4_096 {
            source_registry
                .register(declaration(
                    &format!("demo:channel_{index}"),
                    PayloadDirection::PlayToServer,
                    None,
                ))
                .unwrap();
        }
        let source = PayloadLink::new(source_registry);
        let encoded = source.registration();
        assert!(encoded.len() <= MAX_REGISTRATION);

        let mut target = PayloadLink::default();
        let applied = target.apply_registration(&encoded).unwrap();
        assert!(applied < 4_096);
        assert_eq!(target.registry().declarations().count(), applied);
    }

    #[test]
    fn receive_validates_channel_direction_and_codec() {
        let mut link = link();
        let frame = PayloadFrame::new(
            "demo:to_server",
            PayloadDirection::PlayToServer,
            b"hello".to_vec(),
        )
        .unwrap();
        assert!(
            link.receive(&frame.encode(), PayloadDirection::PlayToServer)
                .is_ok()
        );
        assert_eq!(link.incoming().len(), 1);
        // Wrong direction, unknown channel, invalid UTF-8 and trailing bytes.
        assert!(
            link.receive(&frame.encode(), PayloadDirection::PlayToClient)
                .is_err()
        );
        let unknown =
            PayloadFrame::new("demo:other", PayloadDirection::PlayToServer, Vec::new()).unwrap();
        assert!(
            link.receive(&unknown.encode(), PayloadDirection::PlayToServer)
                .is_err()
        );
        let bad = PayloadFrame::new("demo:to_server", PayloadDirection::PlayToServer, vec![0xff])
            .unwrap();
        assert!(
            link.receive(&bad.encode(), PayloadDirection::PlayToServer)
                .is_err()
        );
        let mut trailing = frame.encode();
        trailing.push(0);
        assert!(
            link.receive(&trailing, PayloadDirection::PlayToServer)
                .is_err()
        );
    }

    #[test]
    fn send_queues_only_declared_valid_frames() {
        let mut link = link();
        let encoded = link
            .send(
                "demo:to_server",
                PayloadDirection::PlayToServer,
                b"ok".to_vec(),
            )
            .unwrap();
        assert_eq!(link.take_outgoing(), vec![encoded.clone()]);
        assert!(PayloadFrame::decode(&encoded).is_ok());
        assert!(
            link.send(
                "demo:to_server",
                PayloadDirection::PlayToClient,
                b"ok".to_vec()
            )
            .is_err()
        );
        assert!(
            link.send("demo:missing", PayloadDirection::PlayToServer, Vec::new())
                .is_err()
        );
        assert!(
            link.send("demo:to_server", PayloadDirection::PlayToServer, vec![0xff])
                .is_err()
        );
    }

    #[test]
    fn bidirectional_declarations_accept_only_play_directions() {
        let mut link = PayloadLink::new(registry([declaration(
            "demo:both",
            PayloadDirection::PlayBidirectional,
            Some("minecraft:empty"),
        )]));
        assert!(link.registry().get("demo:both").is_some());
        assert!(
            link.send("demo:both", PayloadDirection::PlayToServer, Vec::new())
                .is_ok()
        );
        assert!(
            link.send("demo:both", PayloadDirection::PlayToClient, Vec::new())
                .is_ok()
        );
        assert!(
            link.send(
                "demo:both",
                PayloadDirection::ConfigurationToClient,
                Vec::new()
            )
            .is_err()
        );
        let configuration_frame = PayloadFrame::new(
            "demo:both",
            PayloadDirection::ConfigurationToServer,
            Vec::new(),
        )
        .unwrap();
        assert!(
            link.receive(
                &configuration_frame.encode(),
                PayloadDirection::ConfigurationToServer
            )
            .is_err()
        );
        assert!(
            link.send("demo:both", PayloadDirection::PlayToServer, vec![1])
                .is_err()
        );
    }

    #[test]
    fn server_and_client_links_exchange_declared_frames() {
        let declarations = [
            declaration(
                "demo:up",
                PayloadDirection::PlayToServer,
                Some("minecraft:string"),
            ),
            declaration("demo:down", PayloadDirection::PlayToClient, None),
        ];
        let mut server = PayloadLink::new(registry(declarations.clone()));
        let mut client = PayloadLink::default();
        client.apply_registration(&server.registration()).unwrap();
        let up = client
            .send("demo:up", PayloadDirection::PlayToServer, b"hi".to_vec())
            .unwrap();
        let frame = server.receive(&up, PayloadDirection::PlayToServer).unwrap();
        assert_eq!(frame.bytes(), b"hi");
        let down = server
            .send("demo:down", PayloadDirection::PlayToClient, b"ok".to_vec())
            .unwrap();
        client
            .receive(&down, PayloadDirection::PlayToClient)
            .unwrap();
        assert_eq!(client.take_incoming().len(), 1);
        assert!(server.take_incoming().len() == 1);
    }

    #[test]
    fn java_host_frames_publish_through_the_channel_registry() {
        let registry = registry([
            declaration(
                "demo:up",
                PayloadDirection::PlayToServer,
                Some("minecraft:string"),
            ),
            declaration("demo:down", PayloadDirection::PlayToClient, None),
        ]);
        let mut host = horizon_native_java::StdHost::new();
        host.queue_outgoing_payload("demo:up", "playToServer", b"hi".to_vec());
        host.queue_outgoing_payload("demo:down", "playToClient", b"ok".to_vec());
        let frames = publish_from_host_with(&registry, &mut host).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].id(), "demo:up");
        assert_eq!(frames[0].bytes(), b"hi");
        assert!(host.take_outgoing_payloads().is_empty());
        // Undeclared, wrong direction and bad codec all stop the batch.
        let mut bad = horizon_native_java::StdHost::new();
        bad.queue_outgoing_payload("demo:missing", "playToServer", Vec::new());
        assert!(publish_from_host_with(&registry, &mut bad).is_err());
        let mut wrong = horizon_native_java::StdHost::new();
        wrong.queue_outgoing_payload("demo:up", "playToClient", Vec::new());
        assert!(publish_from_host_with(&registry, &mut wrong).is_err());
        let mut codec = horizon_native_java::StdHost::new();
        codec.queue_outgoing_payload("demo:up", "playToServer", vec![0xff]);
        assert!(publish_from_host_with(&registry, &mut codec).is_err());
    }

    #[test]
    fn a_networking_send_queues_the_payload_marker_and_bytes() {
        use horizon_native_java::{Host, MethodRef, Value};
        let mut host = horizon_native_java::StdHost::new()
            .with_packet_type("payload#1", "demo:up")
            .with_payload_bytes("payload#1", b"wire".to_vec());
        let send = MethodRef {
            owner: "net/fabricmc/fabric/api/networking/v1/ServerPlayNetworking".into(),
            name: "send".into(),
            descriptor: "(Ljava/lang/Object;Lnet/minecraft/network/protocol/common/custom/CustomPacketPayload;)V".into(),
        };
        Host::invoke(&mut host, &send, &[Value::Ref(Some("payload#1".into()))]).unwrap();
        let queued = host.take_outgoing_payloads();
        assert_eq!(
            queued,
            vec![(
                "demo:up".to_owned(),
                "playToClient".to_owned(),
                b"wire".to_vec()
            )]
        );
    }

    #[test]
    fn received_frames_reach_the_java_host() {
        let mut host = horizon_native_java::StdHost::new();
        let frame = PayloadFrame::new("demo:up", PayloadDirection::PlayToServer, b"hello".to_vec())
            .unwrap();
        assert_eq!(deliver_to_host(&mut host, [frame]), 1);
        assert_eq!(
            host.take_incoming_payloads(),
            vec![("demo:up".to_owned(), b"hello".to_vec())]
        );
    }

    #[test]
    fn without_stored_jars_no_handler_runs() {
        let _guard = global_state();
        clear_handler_jars();
        let frame =
            PayloadFrame::new("demo:up", PayloadDirection::PlayToServer, b"x".to_vec()).unwrap();
        assert!(!dispatch_received(&frame).unwrap());
    }

    #[test]
    fn supported_codecs_decode_the_payload_value() {
        let string = PayloadDeclaration {
            id: "demo:text".into(),
            direction: PayloadDirection::PlayToServer,
            codec: Some("minecraft:string".into()),
        };
        let frame = PayloadFrame::new(
            "demo:text",
            PayloadDirection::PlayToServer,
            b"hello".to_vec(),
        )
        .unwrap();
        assert_eq!(
            payload_values(Some(&string), &frame).0,
            Value::Ref(Some("hello".into()))
        );
        let empty = PayloadDeclaration {
            id: "demo:empty".into(),
            direction: PayloadDirection::PlayToServer,
            codec: Some("minecraft:unit".into()),
        };
        let frame =
            PayloadFrame::new("demo:empty", PayloadDirection::PlayToServer, Vec::new()).unwrap();
        assert_eq!(
            payload_values(Some(&empty), &frame).0,
            Value::Ref(Some("payload-empty/demo:empty".into()))
        );
        let raw = PayloadDeclaration {
            id: "demo:raw".into(),
            direction: PayloadDirection::PlayToServer,
            codec: Some("minecraft:byte_array".into()),
        };
        let frame =
            PayloadFrame::new("demo:raw", PayloadDirection::PlayToServer, vec![0xff]).unwrap();
        assert_eq!(
            payload_values(Some(&raw), &frame).0,
            Value::Ref(Some("payload-bytes/ff".into()))
        );
    }

    #[test]
    fn dispatch_distinguishes_raw_text_empty_and_markers() {
        let frame = |id: &str, bytes: Vec<u8>| {
            PayloadFrame::new(id, PayloadDirection::PlayToServer, bytes).unwrap()
        };
        // Raw bytes cross as their bounded hex, not the id-carrying marker.
        let raw = declaration(
            "demo:raw",
            PayloadDirection::PlayToServer,
            Some("minecraft:byte_array"),
        );
        assert_eq!(
            payload_values(Some(&raw), &frame("demo:raw", vec![0x00, 0xff])).0,
            Value::Ref(Some("payload-bytes/00ff".into()))
        );
        // An unmodelled codec and an undeclared channel keep the old marker.
        let custom = declaration(
            "demo:custom",
            PayloadDirection::PlayToServer,
            Some("demo:codec"),
        );
        assert_eq!(
            payload_values(Some(&custom), &frame("demo:custom", b"A".to_vec())).0,
            Value::Ref(Some("payload/demo:custom/41".into()))
        );
        assert_eq!(
            payload_values(None, &frame("demo:none", b"A".to_vec())).0,
            Value::Ref(Some("payload/demo:none/41".into()))
        );
        // Invalid UTF-8 falls back to the marker instead of a lossy string.
        let text = declaration(
            "demo:text",
            PayloadDirection::PlayToServer,
            Some("minecraft:string"),
        );
        assert_eq!(
            payload_values(Some(&text), &frame("demo:text", vec![0xff])).0,
            Value::Ref(Some("payload/demo:text/ff".into()))
        );
        // A large raw payload is bounded: 256 bytes of hex, not the whole frame.
        let marker = payload_values(Some(&raw), &frame("demo:raw", vec![0xab; 4096])).0;
        assert_eq!(
            marker,
            Value::Ref(Some(format!("payload-bytes/{}", "ab".repeat(256))))
        );
    }

    #[test]
    fn replacing_declarations_discards_queued_java_frames() {
        let _guard = global_state();
        clear_handler_jars();
        let old = declaration(
            "demo:old",
            PayloadDirection::PlayToServer,
            Some("minecraft:raw"),
        );
        store_declared([old]).unwrap();
        let mut host = horizon_native_java::StdHost::new();
        host.queue_outgoing_payload("demo:old", "playToServer", vec![1]);
        assert_eq!(publish_from_host(&mut host).unwrap(), 1);

        // Replacing the content root invalidates sends queued under its old
        // declarations, so no stale frame can leak into the new session.
        store_declared([declaration(
            "demo:new",
            PayloadDirection::PlayToServer,
            Some("minecraft:raw"),
        )])
        .unwrap();
        assert!(take_java_outgoing().is_empty());
    }

    #[test]
    fn take_incoming_up_to_keeps_the_remainder_queued() {
        let mut server = PayloadLink::new(registry([declaration(
            "demo:to_client",
            PayloadDirection::PlayToClient,
            None,
        )]));
        let mut client = server.clone();
        for n in 0..5u8 {
            let data = server
                .send("demo:to_client", PayloadDirection::PlayToClient, vec![n])
                .unwrap();
            client
                .receive(&data, PayloadDirection::PlayToClient)
                .unwrap();
        }
        let first = client.take_incoming_up_to(3);
        assert_eq!(first.len(), 3);
        assert_eq!(first[0].bytes(), &[0]);
        assert_eq!(client.incoming().len(), 2);
        // Oldest first, and the rest comes out exactly once.
        let rest = client.take_incoming_up_to(100);
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].bytes(), &[3]);
        assert!(client.take_incoming_up_to(100).is_empty());
    }

    #[test]
    fn host_frames_for_the_server_drain_exactly_once_and_stay_bounded() {
        let _guard = global_state();
        clear_handler_jars();
        store_declared([
            declaration("demo:up", PayloadDirection::PlayToServer, None),
            declaration("demo:down", PayloadDirection::PlayToClient, None),
        ])
        .unwrap();
        let mut host = horizon_native_java::StdHost::new();
        host.queue_outgoing_payload("demo:up", "playToServer", vec![1]);
        host.queue_outgoing_payload("demo:down", "playToClient", vec![2]);
        host.queue_outgoing_payload("demo:up", "playToServer", vec![3]);
        assert_eq!(publish_from_host(&mut host).unwrap(), 3);

        // The client drain sees only what travels to the server, in order,
        // and a bounded call leaves the remainder queued.
        let first = take_java_to_server(1);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].bytes(), &[1]);
        let rest = take_java_to_server(256);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].bytes(), &[3]);
        assert!(take_java_to_server(256).is_empty());

        // The shared queue the hub drains is untouched by the client drain.
        let shared = take_java_outgoing();
        assert_eq!(shared.len(), 3);
        assert!(take_java_outgoing().is_empty());

        // Nothing drains the mirror in a process without a client window:
        // the oldest frames are dropped rather than sends failing.
        for round in 0..3u8 {
            let mut host = horizon_native_java::StdHost::new();
            for n in 0..200u8 {
                host.queue_outgoing_payload("demo:up", "playToServer", vec![round, n]);
            }
            publish_from_host(&mut host).unwrap();
            take_java_outgoing();
        }
        let kept = take_java_to_server(usize::MAX);
        assert_eq!(kept.len(), MAX_OUTGOING);
        assert_eq!(kept.last().unwrap().bytes(), &[2, 199]);

        // A new content root discards frames queued under the old one.
        let mut host = horizon_native_java::StdHost::new();
        host.queue_outgoing_payload("demo:up", "playToServer", vec![9]);
        publish_from_host(&mut host).unwrap();
        store_declared([declaration("demo:up", PayloadDirection::PlayToServer, None)]).unwrap();
        assert!(take_java_to_server(256).is_empty());
    }

    // ---- a compiled client mod's handler -------------------------------

    fn javac() -> std::path::PathBuf {
        std::env::var_os("JAVA_HOME")
            .map(|home| std::path::PathBuf::from(home).join("bin/javac"))
            .unwrap_or_else(|| "javac".into())
    }

    fn class_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(root)
            .unwrap()
            .flat_map(|entry| {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    class_files(&path)
                } else {
                    vec![path]
                }
            })
            .collect()
    }

    /// A one-class Fabric client mod: it receives `demo:frame` on
    /// `ClientPlayNetworking` and answers with a `demo:reply` send. Returns
    /// `None` (a skip) when no `javac` is installed.
    fn build_client_mod(root: &std::path::Path) -> Option<std::path::PathBuf> {
        use std::io::Write;
        let sources = [
            (
                "net/fabricmc/api/ClientModInitializer.java",
                "package net.fabricmc.api; public interface ClientModInitializer { void onInitializeClient(); }",
            ),
            (
                "net/minecraft/resources/Identifier.java",
                "package net.minecraft.resources; public final class Identifier { public static Identifier fromNamespaceAndPath(String ns, String path) { return new Identifier(); } }",
            ),
            (
                "net/minecraft/network/codec/StreamCodec.java",
                "package net.minecraft.network.codec; public interface StreamCodec<B, V> {}",
            ),
            (
                "net/minecraft/network/protocol/common/custom/CustomPacketPayload.java",
                "package net.minecraft.network.protocol.common.custom; import net.minecraft.network.codec.StreamCodec; public interface CustomPacketPayload { final class Type<T extends CustomPacketPayload> { public Type(net.minecraft.resources.Identifier id, StreamCodec<?, T> codec) {} } }",
            ),
            (
                "net/fabricmc/fabric/api/client/networking/v1/ClientPlayNetworking.java",
                "package net.fabricmc.fabric.api.client.networking.v1; import net.minecraft.network.protocol.common.custom.CustomPacketPayload; public final class ClientPlayNetworking { public interface PlayPayloadHandler<T extends CustomPacketPayload> { void receive(T payload, Object context); } public static <T extends CustomPacketPayload> boolean registerGlobalReceiver(CustomPacketPayload.Type<T> type, PlayPayloadHandler<T> handler) { return true; } public static void send(CustomPacketPayload payload) {} }",
            ),
            (
                "demo/Mod.java",
                "package demo; import net.fabricmc.api.ClientModInitializer; import net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking; import net.minecraft.network.protocol.common.custom.CustomPacketPayload; import net.minecraft.resources.Identifier; public final class Mod implements ClientModInitializer { public void onInitializeClient() { CustomPacketPayload.Type<CustomPacketPayload> type = new CustomPacketPayload.Type<>(Identifier.fromNamespaceAndPath(\"demo\", \"frame\"), null); ClientPlayNetworking.registerGlobalReceiver(type, this::handle); } private void handle(CustomPacketPayload payload, Object context) { Object reply = new CustomPacketPayload.Type<>(Identifier.fromNamespaceAndPath(\"demo\", \"reply\"), null); ClientPlayNetworking.send((CustomPacketPayload) reply); } }",
            ),
        ];
        let src = root.join("src");
        let classes = root.join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        let mut paths = Vec::new();
        for (relative, body) in sources {
            let path = src.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            paths.push(path);
        }
        let status = match std::process::Command::new(javac())
            .args(["--release", "17", "-d"])
            .arg(&classes)
            .args(&paths)
            .status()
        {
            Ok(status) => status,
            Err(error) => {
                eprintln!("skipping: javac is unavailable ({error})");
                return None;
            }
        };
        assert!(status.success(), "javac failed");
        let jar = root.join("client-payload.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(
            br#"{"id":"client-payload","version":"1.0","entrypoints":{"client":"demo.Mod"}}"#,
        )
        .unwrap();
        for class in class_files(&classes) {
            let entry = class
                .strip_prefix(&classes)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            // Only the mod's class: every stub call must cross the host.
            if !entry.starts_with("demo/Mod") {
                continue;
            }
            zip.start_file(entry, options).unwrap();
            zip.write_all(&std::fs::read(class).unwrap()).unwrap();
        }
        zip.finish().unwrap();
        Some(jar)
    }

    #[test]
    fn a_client_received_frame_reaches_the_client_handler_and_its_reply_queues() {
        let _guard = global_state();
        let root = std::env::temp_dir().join(format!(
            "horizon-client-payload-handler-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let Some(jar_path) = build_client_mod(&root) else {
            return;
        };
        let jar = NativeJar::from_path(&jar_path).unwrap();
        let entry = jar.discover_entrypoint("client").unwrap();
        let mut host = jar.seed_host(StdHost::new());
        jar.execute(&entry.class, entry.method.as_deref(), "client", &mut host)
            .unwrap();
        assert_eq!(host.payload_registrations().len(), 1);
        assert_eq!(host.payload_registrations()[0].direction, "playToClient");

        clear_handler_jars();
        store_declared([
            declaration("demo:frame", PayloadDirection::PlayToClient, None),
            declaration("demo:reply", PayloadDirection::PlayToServer, None),
        ])
        .unwrap();
        let frame = PayloadFrame::new("demo:frame", PayloadDirection::PlayToClient, b"hi".to_vec())
            .unwrap();

        // Stored without its host the registration's marker cannot resolve to
        // a channel id: nothing runs. That is why a host-carrying store exists.
        store_handler_jar(jar.clone(), host.payload_registrations().to_vec());
        assert!(!dispatch_received_as(&frame, PayloadDirection::PlayToClient).unwrap());
        clear_handler_jars();

        store_handler_host(jar, &host);
        // A client-side receiver does not answer a frame received as the
        // server's, and an unrelated channel finds no handler.
        assert!(!dispatch_received_as(&frame, PayloadDirection::PlayToServer).unwrap());
        let other =
            PayloadFrame::new("demo:other", PayloadDirection::PlayToClient, Vec::new()).unwrap();
        assert!(!dispatch_received_as(&other, PayloadDirection::PlayToClient).unwrap());
        assert!(take_java_to_server(256).is_empty());

        assert!(dispatch_received_as(&frame, PayloadDirection::PlayToClient).unwrap());
        let replies = take_java_to_server(256);
        assert_eq!(
            replies.len(),
            1,
            "the handler's send reaches the client queue"
        );
        assert_eq!(replies[0].id(), "demo:reply");
        assert_eq!(replies[0].direction(), PayloadDirection::PlayToServer);
        // Delivered exactly once.
        assert!(take_java_to_server(256).is_empty());

        clear_handler_jars();
        let _ = take_java_outgoing();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The hub's drain takes only the frames a client can receive; the
    /// server-bound ones stay reachable in the client window's mirror.
    #[test]
    fn client_drain_takes_only_client_bound_frames_and_keeps_server_bound_ones() {
        let _guard = global_state();
        clear_handler_jars();
        store_declared([
            declaration("demo:up", PayloadDirection::PlayToServer, None),
            declaration("demo:down", PayloadDirection::PlayToClient, None),
            declaration("demo:both", PayloadDirection::PlayBidirectional, None),
        ])
        .unwrap();
        let mut host = horizon_native_java::StdHost::new();
        host.queue_outgoing_payload("demo:up", "playToServer", vec![1]);
        host.queue_outgoing_payload("demo:down", "playToClient", vec![2]);
        host.queue_outgoing_payload("demo:both", "playToServer", vec![3]);
        host.queue_outgoing_payload("demo:down", "playToClient", vec![4]);
        assert_eq!(publish_from_host(&mut host).unwrap(), 4);

        // A bounded call leaves the rest for the next tick.
        let first = take_java_to_clients(1);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id(), "demo:down");
        assert_eq!(first[0].bytes(), &[2]);
        let rest = take_java_to_clients(256);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].bytes(), &[4]);
        assert!(take_java_to_clients(256).is_empty());

        // The server-bound frames were kept for the client window.
        let to_server = take_java_to_server(256);
        assert_eq!(to_server.len(), 2);
        assert_eq!(to_server[0].bytes(), &[1]);
        assert_eq!(to_server[1].bytes(), &[3]);
        assert!(take_java_outgoing().is_empty());
    }
}
