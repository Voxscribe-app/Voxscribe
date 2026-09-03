//! Ducking of other applications' audio while recording.
//!
//! Volume is changed per output *stream*, never on the sink. Moving the sink
//! volume would pop the desktop's volume OSD on every dictation and, worse,
//! would leave the speakers wrong if Duskr died while ducked. Per-stream
//! changes are invisible to master-volume watchers and revert cleanly.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use pipewire as pw;
use pw::spa::param::ParamType;
use pw::spa::pod::{
    deserialize::PodDeserializer, serialize::PodSerializer, Object, Property, Value, ValueArray,
};
use pw::spa::utils::SpaTypes;
use pw::types::ObjectType;

/// Nodes belonging to Duskr itself; ducking our own ping would restore it to a
/// ducked level once the recording ends.
const OWN_NODES: &[&str] = &["duskr-feedback", "duskr-capture"];

#[derive(Debug, Clone, PartialEq)]
struct NodeVolumes {
    /// Identity beyond the numeric id: PipeWire recycles object ids, so a
    /// stream that ends while ducked could hand its id to an unrelated stream.
    identity: String,
    volumes: Vec<f32>,
}

#[derive(Default)]
struct DuckState {
    /// Latest volumes reported by each live output stream.
    live: HashMap<u32, NodeVolumes>,
    /// Volumes captured at duck time, restored verbatim afterwards.
    saved: HashMap<u32, NodeVolumes>,
    ducked: bool,
}

enum Command {
    Duck(f32),
    Restore,
    Quit,
}

pub struct Ducker {
    sender: Option<pw::channel::Sender<Command>>,
    state: Arc<Mutex<DuckState>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Ducker {
    pub fn start() -> Result<Self> {
        let state = Arc::new(Mutex::new(DuckState::default()));
        let (sender, receiver) = pw::channel::channel::<Command>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();

        let thread_state = Arc::clone(&state);
        let handle = std::thread::Builder::new()
            .name("duskr-ducker".into())
            .spawn(move || {
                if let Err(err) = run_loop(thread_state, receiver, &ready_tx) {
                    let _ = ready_tx.send(Err(err));
                }
            })
            .context("spawning the ducking thread")?;

        ready_rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| anyhow!("PipeWire ducking did not start"))??;

        Ok(Self {
            sender: Some(sender),
            state,
            handle: Some(handle),
        })
    }

    /// Reduce every other stream to `100 - percent` of its current volume.
    pub fn duck(&self, percent: u8) {
        if self.is_ducked() {
            return;
        }
        let multiplier = ((100.0 - percent.min(100) as f32) / 100.0).clamp(0.0, 1.0);
        if let Some(sender) = &self.sender {
            let _ = sender.send(Command::Duck(multiplier));
        }
    }

    pub fn restore(&self) {
        if !self.is_ducked() {
            return;
        }
        if let Some(sender) = &self.sender {
            let _ = sender.send(Command::Restore);
        }
    }

    pub fn is_ducked(&self) -> bool {
        self.state.lock().map(|s| s.ducked).unwrap_or(false)
    }
}

impl Drop for Ducker {
    fn drop(&mut self) {
        // Never leave someone's music quiet because the daemon exited.
        self.restore();
        std::thread::sleep(Duration::from_millis(80));
        if let Some(sender) = &self.sender {
            let _ = sender.send(Command::Quit);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Extract `channelVolumes` from a Props pod.
fn parse_channel_volumes(bytes: &[u8]) -> Option<Vec<f32>> {
    let (_, value) = PodDeserializer::deserialize_any_from(bytes).ok()?;
    let Value::Object(object) = value else {
        return None;
    };
    object.properties.iter().find_map(|property| {
        if property.key != pw::spa::sys::SPA_PROP_channelVolumes {
            return None;
        }
        match &property.value {
            Value::ValueArray(ValueArray::Float(volumes)) => Some(volumes.clone()),
            _ => None,
        }
    })
}

fn build_volume_pod(volumes: &[f32]) -> Result<Vec<u8>> {
    let object = Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties: vec![Property::new(
            pw::spa::sys::SPA_PROP_channelVolumes,
            Value::ValueArray(ValueArray::Float(volumes.to_vec())),
        )],
    };
    Ok(
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
            .map_err(|err| anyhow!("serializing volumes: {err:?}"))?
            .0
            .into_inner(),
    )
}

fn scale(volumes: &[f32], multiplier: f32) -> Vec<f32> {
    volumes
        .iter()
        .map(|v| (v * multiplier).clamp(0.0, 1.0))
        .collect()
}

fn run_loop(
    state: Arc<Mutex<DuckState>>,
    receiver: pw::channel::Receiver<Command>,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    // Bound proxies and their listeners have to outlive the callback that made
    // them, or PipeWire stops delivering their param events.
    let nodes: Rc<RefCell<HashMap<u32, (pw::node::Node, pw::node::NodeListener)>>> =
        Rc::new(RefCell::new(HashMap::new()));

    let registry_for_globals = registry.clone();
    let global_state = Arc::clone(&state);
    let global_nodes = Rc::clone(&nodes);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != ObjectType::Node {
                return;
            }
            let Some(props) = &global.props else { return };
            if props.get("media.class") != Some("Stream/Output/Audio") {
                return;
            }
            let name = props.get("node.name").unwrap_or_default();
            if OWN_NODES.contains(&name) {
                return;
            }

            let Ok(node) = registry_for_globals.bind::<pw::node::Node, _>(global) else {
                return;
            };
            let identity = format!(
                "{}|{}|{}",
                props.get("application.process.id").unwrap_or_default(),
                props.get("application.name").unwrap_or_default(),
                name
            );

            let id = global.id;
            let param_state = Arc::clone(&global_state);
            let listener = node
                .add_listener_local()
                .param(move |_, param_id, _, _, param| {
                    if param_id != ParamType::Props {
                        return;
                    }
                    let Some(pod) = param else { return };
                    let Some(volumes) = parse_channel_volumes(pod.as_bytes()) else {
                        return;
                    };
                    let mut state = param_state.lock().expect("duck state poisoned");
                    // While ducked, the values coming back are our own writes;
                    // recording them would lose the originals.
                    if state.ducked && state.saved.contains_key(&id) {
                        return;
                    }
                    state.live.insert(
                        id,
                        NodeVolumes {
                            identity: identity.clone(),
                            volumes,
                        },
                    );
                })
                .register();

            node.subscribe_params(&[ParamType::Props]);
            global_nodes
                .borrow_mut()
                .insert(global.id, (node, listener));
        })
        .global_remove({
            let state = Arc::clone(&state);
            let nodes = Rc::clone(&nodes);
            move |id| {
                nodes.borrow_mut().remove(&id);
                let mut state = state.lock().expect("duck state poisoned");
                state.live.remove(&id);
                state.saved.remove(&id);
            }
        })
        .register();

    let _ = ready.send(Ok(()));

    let loop_handle = mainloop.clone();
    let command_state = Arc::clone(&state);
    let command_nodes = Rc::clone(&nodes);
    let _receiver = receiver.attach(mainloop.loop_(), move |command| match command {
        Command::Duck(multiplier) => {
            let mut state = command_state.lock().expect("duck state poisoned");
            if state.ducked {
                return;
            }
            state.saved = state.live.clone();
            state.ducked = true;
            let targets = state.saved.clone();
            drop(state);

            let nodes = command_nodes.borrow();
            let mut count = 0usize;
            for (id, entry) in &targets {
                let Some((node, _)) = nodes.get(id) else {
                    continue;
                };
                let Ok(pod) = build_volume_pod(&scale(&entry.volumes, multiplier)) else {
                    continue;
                };
                if let Some(pod) = pw::spa::pod::Pod::from_bytes(&pod) {
                    node.set_param(ParamType::Props, 0, pod);
                    count += 1;
                }
            }
            if count > 0 {
                tracing::debug!("ducked {count} stream(s)");
            }
        }
        Command::Restore => {
            let mut state = command_state.lock().expect("duck state poisoned");
            if !state.ducked {
                return;
            }
            let targets = std::mem::take(&mut state.saved);
            state.ducked = false;
            drop(state);

            let nodes = command_nodes.borrow();
            for (id, entry) in &targets {
                let Some((node, _)) = nodes.get(id) else {
                    continue;
                };
                // Identity guard: if this id now belongs to a different stream,
                // restoring would set a volume the user never chose.
                let still_ours = command_state
                    .lock()
                    .expect("duck state poisoned")
                    .live
                    .get(id)
                    .map(|live| live.identity == entry.identity)
                    .unwrap_or(true);
                if !still_ours {
                    continue;
                }
                let Ok(pod) = build_volume_pod(&entry.volumes) else {
                    continue;
                };
                if let Some(pod) = pw::spa::pod::Pod::from_bytes(&pod) {
                    node.set_param(ParamType::Props, 0, pod);
                }
            }
        }
        Command::Quit => loop_handle.quit(),
    });

    mainloop.run();
    Ok(())
}

use std::cell::RefCell;
use std::rc::Rc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_pods_round_trip_through_the_parser() {
        let volumes = vec![0.4, 0.4];
        let pod = build_volume_pod(&volumes).unwrap();
        assert_eq!(parse_channel_volumes(&pod), Some(volumes));
    }

    #[test]
    fn a_pod_without_channel_volumes_yields_nothing() {
        let object = Object {
            type_: SpaTypes::ObjectParamProps.as_raw(),
            id: ParamType::Props.as_raw(),
            properties: vec![Property::new(
                pw::spa::sys::SPA_PROP_mute,
                Value::Bool(true),
            )],
        };
        let bytes =
            PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
                .unwrap()
                .0
                .into_inner();
        assert_eq!(parse_channel_volumes(&bytes), None);
    }

    #[test]
    fn ducking_by_fifty_percent_halves_each_channel() {
        assert_eq!(scale(&[1.0, 0.5], 0.5), vec![0.5, 0.25]);
    }

    #[test]
    fn scaled_volumes_never_leave_the_valid_range() {
        assert_eq!(scale(&[2.0], 1.0), vec![1.0]);
        assert_eq!(scale(&[1.0], 0.0), vec![0.0]);
    }

    #[test]
    fn duskr_own_nodes_are_never_ducked() {
        assert!(OWN_NODES.contains(&"duskr-feedback"));
        assert!(OWN_NODES.contains(&"duskr-capture"));
    }
}
