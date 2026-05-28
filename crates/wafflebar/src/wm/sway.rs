//! sway / i3 backend: the i3-ipc protocol over `$SWAYSOCK` (or `$I3SOCK` — same wire format).
//!
//! Two connections, the swaybar idiom: an **event stream** (`SUBSCRIBE`d to workspace/window/output,
//! its fd watched by the host) stays clean of command replies, and a **query/command** connection
//! for request-reply (`GET_WORKSPACES`/`GET_TREE`/`RUN_COMMAND`). On any event we re-query and rebuild
//! the `WmEvent`s — the event is the *trigger*, the query is the *truth* (a fast user may have changed
//! state again before we read). The data model is dwl's: sway's named workspaces map to
//! `Tag{index, name}` (this validated the WindowManager trait against a second compositor — see PR-A).

use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

use anyhow::{bail, Result};
use serde::Deserialize;
use tracing::{debug, warn};
use wafflebar_core::{Tag, TagState, Window, WindowId, WindowManager, WmCommand, WmEvent};

use super::WmConnection;

// --- i3-ipc framing codec (pure bytes ↔ messages; the unit-tested core) ---

const MAGIC: &[u8; 6] = b"i3-ipc";
const HEADER_LEN: usize = MAGIC.len() + 4 + 4; // magic + u32 length + u32 type
const EVENT_BIT: u32 = 0x8000_0000; // event reply types have the high bit set

// Message types we send (replies share the type; events OR in EVENT_BIT).
const RUN_COMMAND: u32 = 0;
const GET_WORKSPACES: u32 = 1;
const SUBSCRIBE: u32 = 2;
const GET_TREE: u32 = 4;

#[derive(Debug, PartialEq, Eq)]
enum CodecError {
    BadMagic,
}

/// Frame a message: magic + native-endian length + native-endian type + payload.
fn encode(msg_type: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    buf.extend_from_slice(&msg_type.to_ne_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Parse one message from the front of `buf`: `Some((type, payload, consumed))`, `None` if the buffer
/// doesn't yet hold a complete message, `Err` on a bad magic (caller resyncs).
fn decode(buf: &[u8]) -> Result<Option<(u32, Vec<u8>, usize)>, CodecError> {
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    if &buf[..MAGIC.len()] != MAGIC {
        return Err(CodecError::BadMagic);
    }
    let len = u32::from_ne_bytes(buf[6..10].try_into().unwrap()) as usize;
    let msg_type = u32::from_ne_bytes(buf[10..14].try_into().unwrap());
    if buf.len() < HEADER_LEN + len {
        return Ok(None); // payload not fully arrived
    }
    Ok(Some((msg_type, buf[HEADER_LEN..HEADER_LEN + len].to_vec(), HEADER_LEN + len)))
}

// --- i3-ipc reply shapes (only the fields we use) ---

#[derive(Deserialize)]
struct SwayWorkspace {
    name: String,
    focused: bool,
    urgent: bool,
    output: String,
}

#[derive(Deserialize)]
struct Node {
    id: i64,
    name: Option<String>,
    #[serde(rename = "type")]
    node_type: String,
    app_id: Option<String>,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    floating_nodes: Vec<Node>,
    window_properties: Option<WindowProperties>,
}

#[derive(Deserialize)]
struct WindowProperties {
    class: Option<String>, // XWayland fallback for app_id
}

// --- backend ---

pub struct SwayBackend {
    /// Subscribed event stream; its fd is what the host watches. Non-blocking.
    events: UnixStream,
    /// Request-reply connection for queries/commands. Blocking.
    query: UnixStream,
    /// Partial bytes from the event stream awaiting a complete frame.
    read_buf: Vec<u8>,
    /// Derived state, cached so `snapshot()` (`&self`) needs no query and `execute` can map a
    /// `FocusTag` index back to a workspace name.
    tags_by_output: Vec<(String, Vec<Tag>)>,
    windows: Vec<Window>,
    active: Vec<(String, String, String)>, // (output, title, app_id) per output
}

impl SwayBackend {
    pub fn connect() -> Result<Self> {
        let Some(path) = std::env::var_os("SWAYSOCK").or_else(|| std::env::var_os("I3SOCK")) else {
            bail!("neither SWAYSOCK nor I3SOCK set");
        };
        let mut events = UnixStream::connect(&path)?;
        // Subscribe, then consume the (blocking) subscribe reply before going non-blocking, so the
        // event stream afterwards carries only events.
        events.write_all(&encode(SUBSCRIBE, br#"["workspace","window","output"]"#))?;
        read_reply(&mut events)?;
        events.set_nonblocking(true)?;

        let query = UnixStream::connect(&path)?;
        let mut backend = Self {
            events,
            query,
            read_buf: Vec::new(),
            tags_by_output: Vec::new(),
            windows: Vec::new(),
            active: Vec::new(),
        };
        backend.refresh(); // seed the cache so the first snapshot has state
        debug!("sway backend connected via {}", path.to_string_lossy());
        Ok(backend)
    }

    /// Request-reply on the query connection.
    fn request(&mut self, msg_type: u32, payload: &[u8]) -> io::Result<Vec<u8>> {
        self.query.write_all(&encode(msg_type, payload))?;
        read_reply(&mut self.query)
    }

    /// Re-query workspaces + tree, rebuild the cached `WmEvent`s, and return them.
    fn refresh(&mut self) -> Vec<WmEvent> {
        let workspaces: Vec<SwayWorkspace> = self
            .request(GET_WORKSPACES, b"")
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let tree: Option<Node> =
            self.request(GET_TREE, b"").ok().and_then(|b| serde_json::from_slice(&b).ok());

        // Walk the tree once: windows, the focused leaf, and which workspaces hold windows.
        let mut windows = Vec::new();
        let mut occupied = std::collections::HashSet::new();
        let mut active: Vec<(String, String, String)> = Vec::new();
        if let Some(root) = &tree {
            walk(root, "", "", &mut windows, &mut occupied, &mut active);
        }

        // Group workspaces by output → tags (index = position within the output).
        let mut tags_by_output: Vec<(String, Vec<Tag>)> = Vec::new();
        for ws in &workspaces {
            let state = if ws.urgent {
                TagState::Urgent
            } else if ws.focused {
                TagState::Active
            } else {
                TagState::None
            };
            let tag = Tag {
                index: 0, // assigned below, per output
                name: ws.name.clone(),
                state,
                focused: ws.focused,
                occupied: occupied.contains(&ws.name),
            };
            match tags_by_output.iter_mut().find(|(o, _)| o == &ws.output) {
                Some((_, tags)) => tags.push(tag),
                None => tags_by_output.push((ws.output.clone(), vec![tag])),
            }
        }
        for (_, tags) in &mut tags_by_output {
            for (i, tag) in tags.iter_mut().enumerate() {
                tag.index = i as u32;
            }
        }

        self.tags_by_output = tags_by_output;
        self.windows = windows;
        self.active = active;
        self.cached_events()
    }

    /// The cached state expressed as the events a fresh subscriber needs.
    fn cached_events(&self) -> Vec<WmEvent> {
        let mut events = Vec::new();
        for (output, tags) in &self.tags_by_output {
            events.push(WmEvent::Tags { output: output.clone(), tags: tags.clone() });
        }
        for (output, title, app_id) in &self.active {
            events.push(WmEvent::ActiveWindow {
                output: output.clone(),
                title: title.clone(),
                app_id: app_id.clone(),
            });
        }
        events.push(WmEvent::Windows { windows: self.windows.clone() });
        events
    }
}

impl WindowManager for SwayBackend {
    fn snapshot(&self) -> Vec<WmEvent> {
        self.cached_events()
    }

    fn execute(&mut self, cmd: &WmCommand) {
        let command = match cmd {
            WmCommand::FocusTag { output, tag } => {
                // Map the index back to the workspace name on that output (sway focuses by name).
                let name = self
                    .tags_by_output
                    .iter()
                    .find(|(o, _)| o == output)
                    .and_then(|(_, tags)| tags.get(*tag as usize))
                    .map(|t| t.name.clone());
                match name {
                    Some(name) => format!("workspace \"{name}\""),
                    None => return,
                }
            }
            WmCommand::ActivateWindow(id) => format!("[con_id={id}] focus"),
            WmCommand::CloseWindow(id) => format!("[con_id={id}] kill"),
            // sway/i3 have no minimize concept (scratchpad is the nearest; deferred). No-op.
            WmCommand::SetMinimized(_, _) => return,
            // Sway layout selection and show-desktop aren't wired in v1.
            WmCommand::SetLayout { .. } | WmCommand::ToggleShowDesktop => return,
        };
        if let Err(e) = self.request(RUN_COMMAND, command.as_bytes()) {
            warn!(error = %e, command, "sway: RUN_COMMAND failed");
        }
    }
}

impl WmConnection for SwayBackend {
    fn fd(&self) -> RawFd {
        self.events.as_raw_fd()
    }

    fn dispatch(&mut self) -> Vec<WmEvent> {
        // Drain the non-blocking event stream into the buffer.
        let mut tmp = [0u8; 4096];
        loop {
            match self.events.read(&mut tmp) {
                Ok(0) => break, // EOF (sway exited) — TODO(sway): reconnect with backoff
                Ok(n) => self.read_buf.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    warn!(error = %e, "sway: event stream read error");
                    break;
                }
            }
        }
        // Consume complete frames; any event triggers a re-query (event = trigger, query = truth).
        let mut got_event = false;
        loop {
            match decode(&self.read_buf) {
                Ok(Some((msg_type, _, consumed))) => {
                    self.read_buf.drain(..consumed);
                    got_event |= msg_type & EVENT_BIT != 0;
                }
                Ok(None) => break,
                Err(_) => {
                    self.read_buf.clear(); // resync on corruption rather than wedge
                    break;
                }
            }
        }
        if got_event {
            self.refresh()
        } else {
            Vec::new()
        }
    }
}

/// Read exactly one framed reply (blocking) from `stream`.
fn read_reply(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut header = [0u8; HEADER_LEN];
    stream.read_exact(&mut header)?;
    if &header[..MAGIC.len()] != MAGIC {
        return Err(io::Error::new(ErrorKind::InvalidData, "i3-ipc: bad reply magic"));
    }
    let len = u32::from_ne_bytes(header[6..10].try_into().unwrap()) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}

/// Walk the node tree collecting windows (leaf cons), the focused leaf (as `active` per output), and
/// the set of workspaces that hold windows (for `Tag.occupied`). `output`/`workspace` are the names
/// of the ancestor output/workspace nodes.
fn walk(
    node: &Node,
    output: &str,
    workspace: &str,
    windows: &mut Vec<Window>,
    occupied: &mut std::collections::HashSet<String>,
    active: &mut Vec<(String, String, String)>,
) {
    let (output, workspace) = match node.node_type.as_str() {
        "output" => (node.name.as_deref().unwrap_or(output), workspace),
        "workspace" => (output, node.name.as_deref().unwrap_or(workspace)),
        _ => (output, workspace),
    };

    let is_leaf = node.nodes.is_empty()
        && node.floating_nodes.is_empty()
        && matches!(node.node_type.as_str(), "con" | "floating_con");
    if is_leaf {
        let app_id = node
            .app_id
            .clone()
            .or_else(|| node.window_properties.as_ref().and_then(|w| w.class.clone()))
            .unwrap_or_default();
        let title = node.name.clone().unwrap_or_default();
        if !workspace.is_empty() {
            occupied.insert(workspace.to_string());
        }
        if node.focused {
            active.push((output.to_string(), title.clone(), app_id.clone()));
        }
        windows.push(Window {
            id: node.id as WindowId,
            title,
            app_id,
            focused: node.focused,
            minimized: false,
            outputs: if output.is_empty() { Vec::new() } else { vec![output.to_string()] },
        });
        return;
    }
    for child in node.nodes.iter().chain(&node.floating_nodes) {
        walk(child, output, workspace, windows, occupied, active);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_round_trips_and_handles_partials() {
        let framed = encode(GET_WORKSPACES, b"[]");
        let (ty, payload, consumed) = decode(&framed).unwrap().unwrap();
        assert_eq!(ty, GET_WORKSPACES);
        assert_eq!(payload, b"[]");
        assert_eq!(consumed, framed.len());

        // Truncated header / payload → incomplete (need more bytes), not an error.
        assert_eq!(decode(&framed[..5]).unwrap(), None);
        assert_eq!(decode(&framed[..HEADER_LEN + 1]).unwrap(), None);

        // Two concatenated messages parse one at a time.
        let mut two = encode(SUBSCRIBE, b"a");
        two.extend_from_slice(&encode(GET_TREE, b"bb"));
        let (_, _, consumed) = decode(&two).unwrap().unwrap();
        let (ty2, p2, _) = decode(&two[consumed..]).unwrap().unwrap();
        assert_eq!(ty2, GET_TREE);
        assert_eq!(p2, b"bb");

        // Bad magic → error (caller resyncs).
        assert_eq!(decode(b"nope-it00000000").err(), Some(CodecError::BadMagic));
    }

    #[test]
    fn event_bit_distinguishes_events_from_replies() {
        assert_eq!(GET_WORKSPACES & EVENT_BIT, 0); // a reply type
        assert_ne!(EVENT_BIT & EVENT_BIT, 0); // an event type (EVENT_BIT | 0 = "workspace")
    }

    #[test]
    fn tree_walk_collects_windows_focused_and_occupied() {
        let json = r#"{
            "id": 1, "type": "root", "nodes": [
              {"id": 2, "type": "output", "name": "DP-1", "nodes": [
                {"id": 3, "type": "workspace", "name": "2", "nodes": [
                  {"id": 10, "type": "con", "name": "Firefox", "app_id": "firefox", "focused": true},
                  {"id": 11, "type": "con", "name": "Term", "app_id": "Alacritty", "focused": false}
                ]}
              ]}
            ]
        }"#;
        let root: Node = serde_json::from_str(json).unwrap();
        let (mut windows, mut occ, mut active) =
            (Vec::new(), std::collections::HashSet::new(), Vec::new());
        walk(&root, "", "", &mut windows, &mut occ, &mut active);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].id, 10);
        assert_eq!(windows[0].app_id, "firefox");
        assert_eq!(windows[0].outputs, vec!["DP-1"]);
        assert!(occ.contains("2"));
        assert_eq!(active, vec![("DP-1".into(), "Firefox".into(), "firefox".into())]);
    }
}
