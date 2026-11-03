//! Bounded transport for mod payload channels (`horizon-mods::payloads`).
//!
//! A mod declares channels during bootstrap; the server sends the declaration
//! list on `horizon:payload_channels`, and both sides exchange frames on
//! `horizon:payload`. A frame is only accepted when its id is declared, the
//! direction matches, and a supported codec validates its bytes. Transport is
//! deliberately separate from the packet loop so both the hub and the remote
//! client can drive the same state machine.

use horizon_mods::payloads::{
    PayloadDeclaration, PayloadDirection, PayloadFrame, PayloadRegistry, validate_payload,
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
        let declarations: Vec<&PayloadDeclaration> = self.registry.declarations().collect();
        out.extend_from_slice(&(declarations.len().min(u16::MAX as usize) as u16).to_be_bytes());
        for declaration in declarations.into_iter().take(u16::MAX as usize) {
            out.extend_from_slice(&(declaration.id.len() as u16).to_be_bytes());
            out.extend_from_slice(declaration.id.as_bytes());
            out.push(declaration.direction.wire());
            match &declaration.codec {
                Some(codec) => {
                    out.push(1);
                    out.extend_from_slice(&(codec.len() as u16).to_be_bytes());
                    out.extend_from_slice(codec.as_bytes());
                }
                None => out.push(0),
            }
        }
        out.truncate(MAX_REGISTRATION);
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
    declared == direction || declared == PayloadDirection::PlayBidirectional
}

/// Frames a Java host asked to send, waiting for the server's next tick.
static JAVA_OUTGOING: Mutex<Vec<PayloadFrame>> = Mutex::new(Vec::new());

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
        queue.extend(frames);
    }
    Ok(count)
}

/// Frames Java mods asked to send, drained in order.
pub fn take_java_outgoing() -> Vec<PayloadFrame> {
    std::mem::take(&mut JAVA_OUTGOING.lock().unwrap_or_else(|e| e.into_inner()))
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
    fn bidirectional_declarations_accept_both_play_directions() {
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
}
