use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

pub const TEXT_MIME: &str = "text/plain;charset=utf-8";

const READ_MIMES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub mime: String,
    pub data: Vec<u8>,
}

impl Selection {
    pub fn text(text: &str) -> Self {
        Self {
            mime: TEXT_MIME.to_string(),
            data: text.as_bytes().to_vec(),
        }
    }
}

#[derive(Default)]
struct State {
    manager: Option<ZwlrDataControlManagerV1>,
    seat: Option<WlSeat>,
    offer_mimes: Vec<(ZwlrDataControlOfferV1, Vec<String>)>,
    current_offer: Option<Option<ZwlrDataControlOfferV1>>,
    source_cancelled: bool,
    serving: Option<Arc<Selection>>,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwlr_data_control_manager_v1" => {
                    state.manager = Some(registry.bind::<ZwlrDataControlManagerV1, _, _>(
                        name,
                        version.min(2),
                        qh,
                        (),
                    ));
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, version.min(7), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: <ZwlrDataControlDeviceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::DataOffer { id } => {
                state.offer_mimes.push((id, Vec::new()));
            }
            zwlr_data_control_device_v1::Event::Selection { id } => {
                state.current_offer = Some(id);
            }
            zwlr_data_control_device_v1::Event::Finished => {
                state.current_offer = Some(None);
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for State {
    fn event(
        state: &mut Self,
        offer: &ZwlrDataControlOfferV1,
        event: <ZwlrDataControlOfferV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            if let Some(entry) = state.offer_mimes.iter_mut().find(|(o, _)| o == offer) {
                entry.1.push(mime_type);
            }
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlSourceV1,
        event: <ZwlrDataControlSourceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { mime_type, fd } => {
                let Some(selection) = state.serving.clone() else {
                    return;
                };
                if mime_matches(&mime_type, &selection.mime) || is_text_mime(&mime_type) {
                    write_fd(fd, &selection.data);
                } else {
                    drop(fd);
                }
            }
            zwlr_data_control_source_v1::Event::Cancelled => {
                state.source_cancelled = true;
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore ZwlrDataControlManagerV1);
delegate_noop!(State: ignore WlSeat);

fn is_text_mime(mime: &str) -> bool {
    READ_MIMES.iter().any(|m| m.eq_ignore_ascii_case(mime)) || mime.starts_with("text/")
}

fn mime_matches(requested: &str, offered: &str) -> bool {
    requested.eq_ignore_ascii_case(offered)
}

fn write_fd(fd: OwnedFd, data: &[u8]) {
    let mut file = std::fs::File::from(fd);
    let _ = file.write_all(data);
    let _ = file.flush();
}

struct Session {
    queue: EventQueue<State>,
    state: State,
    device: ZwlrDataControlDeviceV1,
    qh: QueueHandle<State>,
}

impl Session {
    fn connect() -> Result<Self> {
        let connection =
            Connection::connect_to_env().context("connecting to the Wayland display")?;
        let display = connection.display();
        let mut queue: EventQueue<State> = connection.new_event_queue();
        let qh = queue.handle();
        let _registry = display.get_registry(&qh, ());

        let mut state = State::default();
        queue
            .roundtrip(&mut state)
            .context("Wayland registry roundtrip")?;

        let manager = state.manager.clone().ok_or_else(|| {
            anyhow!(
                "compositor does not implement zwlr_data_control_manager_v1; \
                 clipboard fallback is unavailable"
            )
        })?;
        let seat = state
            .seat
            .clone()
            .ok_or_else(|| anyhow!("no wl_seat advertised"))?;

        let device = manager.get_data_device(&seat, &qh, ());
        queue
            .roundtrip(&mut state)
            .context("data-device roundtrip")?;

        Ok(Self {
            queue,
            state,
            device,
            qh,
        })
    }

    fn read_selection(&mut self) -> Result<Option<Selection>> {
        self.queue.roundtrip(&mut self.state)?;

        let Some(offer) = self.state.current_offer.clone().flatten() else {
            return Ok(None);
        };
        let mimes = self
            .state
            .offer_mimes
            .iter()
            .find(|(o, _)| *o == offer)
            .map(|(_, mimes)| mimes.clone())
            .unwrap_or_default();

        let Some(mime) = READ_MIMES
            .iter()
            .find(|wanted| mimes.iter().any(|m| m.eq_ignore_ascii_case(wanted)))
            .map(|m| m.to_string())
            .or_else(|| mimes.first().cloned())
        else {
            return Ok(None);
        };

        let (read_fd, write_fd) = pipe()?;
        offer.receive(mime.clone(), write_fd.as_fd());
        drop(write_fd);
        self.queue.flush()?;

        let mut file = std::fs::File::from(read_fd);
        let mut data = Vec::new();
        file.read_to_end(&mut data)
            .context("reading clipboard data")?;

        Ok(Some(Selection { mime, data }))
    }

    fn set_selection(&mut self, selection: Selection, serve_for: Duration) -> Result<()> {
        let source = self
            .state
            .manager
            .clone()
            .expect("manager bound")
            .create_data_source(&self.qh, ());
        source.offer(selection.mime.clone());
        for alias in READ_MIMES {
            if !alias.eq_ignore_ascii_case(&selection.mime) {
                source.offer((*alias).to_string());
            }
        }
        self.state.serving = Some(Arc::new(selection));
        self.state.source_cancelled = false;
        self.device.set_selection(Some(&source));
        self.queue.flush()?;

        let deadline = Instant::now() + serve_for;
        while Instant::now() < deadline && !self.state.source_cancelled {
            self.queue.blocking_dispatch(&mut self.state)?;
        }
        Ok(())
    }
}

fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a valid two-element array for the duration of the call.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error()).context("creating a clipboard pipe");
    }
    use std::os::fd::FromRawFd;
    // SAFETY: pipe2 succeeded, so both descriptors are open and owned by us.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

pub fn read() -> Result<Option<Selection>> {
    Session::connect()?.read_selection()
}

pub struct ClipboardOwner {
    stop: Arc<Mutex<bool>>,
}

impl ClipboardOwner {
    pub fn release(self) {}
}

impl Drop for ClipboardOwner {
    fn drop(&mut self) {
        if let Ok(mut stop) = self.stop.lock() {
            *stop = true;
        }
    }
}

pub fn set_for(selection: Selection, serve_for: Duration) -> Result<ClipboardOwner> {
    let stop = Arc::new(Mutex::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();

    std::thread::Builder::new()
        .name("voxscribe-clipboard".into())
        .spawn(move || {
            let mut session = match Session::connect() {
                Ok(session) => {
                    let _ = ready_tx.send(Ok(()));
                    session
                }
                Err(err) => {
                    let _ = ready_tx.send(Err(err));
                    return;
                }
            };
            if let Err(err) = session.set_selection(selection, serve_for) {
                tracing::warn!("clipboard ownership ended: {err}");
            }
        })
        .context("spawning the clipboard thread")?;

    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| anyhow!("clipboard thread did not start"))??;

    Ok(ClipboardOwner { stop })
}

pub fn available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() && Session::connect().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_selections_use_the_utf8_mime() {
        let selection = Selection::text("hello");
        assert_eq!(selection.mime, TEXT_MIME);
        assert_eq!(selection.data, b"hello");
    }

    #[test]
    fn text_mimes_are_recognized_case_insensitively() {
        assert!(is_text_mime("TEXT/PLAIN;CHARSET=UTF-8"));
        assert!(is_text_mime("text/html"));
        assert!(!is_text_mime("image/png"));
    }

    #[test]
    fn a_pipe_round_trips_its_payload() {
        let (read_fd, write_fd) = pipe().unwrap();
        write_fd_helper(write_fd, b"payload");
        let mut file = std::fs::File::from(read_fd);
        let mut out = Vec::new();
        file.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"payload");
    }

    fn write_fd_helper(fd: OwnedFd, data: &[u8]) {
        super::write_fd(fd, data);
    }
}
