//! `zwlr_layer_shell_v1` overlay surface. KWin, Hyprland, Sway and Niri all
//! implement it, so one path covers every desktop Duskr's shortcuts reach;
//! GNOME does not, and the OSD reports itself unavailable there.
//!
//! The surface exists only while something is on screen.

use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};
use std::ptr::NonNull;

use anyhow::{anyhow, Context, Result};
use wayland_client::globals::{registry_queue_init, GlobalList, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_output::{self, WlOutput},
    wl_region::WlRegion,
    wl_registry::WlRegistry,
    wl_shm::{Format, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::{self, WlSurface},
};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use super::render::Canvas;
use crate::core::config::OsdPosition;

/// Enough to keep drawing while the compositor holds the committed frame.
const SLOTS: usize = 2;

/// `memfd` mapping shared with the compositor, carved into [`SLOTS`] frames.
struct Pool {
    pool: WlShmPool,
    buffers: [WlBuffer; SLOTS],
    memory: NonNull<u8>,
    len: usize,
    slot_len: usize,
    _file: File,
}

impl Pool {
    fn new(shm: &WlShm, qh: &QueueHandle<State>, width: u32, height: u32) -> Result<Self> {
        let stride = width * 4;
        let slot_len = (stride * height) as usize;
        let len = slot_len * SLOTS;

        let fd = unsafe { libc::memfd_create(c"duskr-osd".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("creating the OSD shm file");
        }
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(len as u64)
            .context("sizing the OSD shm file")?;

        let memory = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if memory == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error()).context("mapping the OSD shm file");
        }
        let memory = NonNull::new(memory as *mut u8).ok_or_else(|| anyhow!("null mapping"))?;

        let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffers = std::array::from_fn(|slot| {
            pool.create_buffer(
                (slot * slot_len) as i32,
                width as i32,
                height as i32,
                stride as i32,
                Format::Argb8888,
                qh,
                slot,
            )
        });

        Ok(Self {
            pool,
            buffers,
            memory,
            len,
            slot_len,
            _file: file,
        })
    }

    fn write(&mut self, slot: usize, bytes: &[u8]) {
        let len = bytes.len().min(self.slot_len);
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.memory.as_ptr().add(slot * self.slot_len),
                len,
            );
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        for buffer in &self.buffers {
            buffer.destroy();
        }
        self.pool.destroy();
        unsafe {
            libc::munmap(self.memory.as_ptr() as *mut libc::c_void, self.len);
        }
    }
}

/// A mapped layer surface plus the buffers backing it.
struct Mapped {
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    pool: Pool,
    /// Device pixels, so a scale change forces a rebuild.
    size: (u32, u32),
    configured: bool,
    next_slot: usize,
}

impl Drop for Mapped {
    fn drop(&mut self) {
        self.layer.destroy();
        self.surface.destroy();
    }
}

struct State {
    compositor: WlCompositor,
    shm: WlShm,
    layer_shell: ZwlrLayerShellV1,
    /// Scale of every output we have seen, in bind order.
    output_scales: Vec<(WlOutput, u32)>,
    /// Outputs the surface is currently on; the largest scale wins.
    entered: Vec<WlOutput>,
    mapped: Option<Mapped>,
    released: [bool; SLOTS],
    /// Set when the compositor asks us to go away.
    closed: bool,
    /// A configure arrived, so the pending frame has to be redrawn.
    dirty: bool,
}

impl State {
    fn scale(&self) -> u32 {
        self.entered
            .iter()
            .filter_map(|output| {
                self.output_scales
                    .iter()
                    .find(|(candidate, _)| candidate == output)
                    .map(|(_, scale)| *scale)
            })
            .max()
            .unwrap_or(1)
            .max(1)
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        output: &WlOutput,
        event: <WlOutput as Proxy>::Event,
        _: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Scale { factor } = event {
            let factor = factor.max(1) as u32;
            match state
                .output_scales
                .iter_mut()
                .find(|(candidate, _)| candidate == output)
            {
                Some(entry) => entry.1 = factor,
                None => state.output_scales.push((output.clone(), factor)),
            }
            state.dirty = true;
        }
    }
}

impl Dispatch<WlSurface, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: <WlSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_surface::Event::Enter { output } => {
                if !state.entered.contains(&output) {
                    state.entered.push(output);
                    state.dirty = true;
                }
            }
            wl_surface::Event::Leave { output } => {
                state.entered.retain(|candidate| *candidate != output);
                state.dirty = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, usize> for State {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: <WlBuffer as Proxy>::Event,
        slot: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wayland_client::protocol::wl_buffer::Event::Release) {
            if let Some(free) = state.released.get_mut(*slot) {
                *free = true;
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                if let Some(mapped) = &mut state.mapped {
                    mapped.configured = true;
                }
                state.dirty = true;
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.closed = true;
                state.mapped = None;
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlRegion);
delegate_noop!(State: ignore ZwlrLayerShellV1);

/// Placement and geometry the compositor needs, in logical pixels.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub position: OsdPosition,
    pub margin: u32,
    pub width: u32,
    pub height: u32,
}

/// Owns the Wayland connection for the OSD thread.
pub struct Window {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    placement: Placement,
}

impl Window {
    /// Fails when there is no Wayland session or no layer shell; the caller
    /// treats that as "no native OSD here", not a startup failure.
    pub fn open(placement: Placement) -> Result<Self> {
        let connection = Connection::connect_to_env().context("connecting to Wayland")?;
        let (globals, queue): (GlobalList, EventQueue<State>) =
            registry_queue_init(&connection).context("reading the Wayland registry")?;
        let qh = queue.handle();

        let compositor: WlCompositor = globals
            .bind(&qh, 1..=6, ())
            .map_err(|err| anyhow!("wl_compositor unavailable: {err}"))?;
        let shm: WlShm = globals
            .bind(&qh, 1..=2, ())
            .map_err(|err| anyhow!("wl_shm unavailable: {err}"))?;
        let layer_shell: ZwlrLayerShellV1 = globals
            .bind(&qh, 1..=4, ())
            .map_err(|_| anyhow!("compositor has no zwlr_layer_shell_v1"))?;

        // Bound purely to learn their scale factors.
        let names: Vec<(u32, u32)> = globals.contents().with_list(|list| {
            list.iter()
                .filter(|global| global.interface == WlOutput::interface().name)
                .map(|global| (global.name, global.version.clamp(2, 3)))
                .collect()
        });
        let output_scales: Vec<(WlOutput, u32)> = names
            .into_iter()
            .enumerate()
            .map(|(index, (name, version))| {
                let output = globals
                    .registry()
                    .bind::<WlOutput, usize, State>(name, version, &qh, index);
                (output, 1)
            })
            .collect();

        Ok(Self {
            connection,
            queue,
            state: State {
                compositor,
                shm,
                layer_shell,
                output_scales,
                entered: Vec::new(),
                mapped: None,
                released: [true; SLOTS],
                closed: false,
                dirty: false,
            },
            placement,
        })
    }

    pub fn scale(&self) -> u32 {
        self.state.scale()
    }

    pub fn is_mapped(&self) -> bool {
        self.state.mapped.is_some()
    }

    /// Polled so the thread can wait on Wayland and its control channel at
    /// once.
    pub fn fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.connection.as_fd().as_raw_fd()
    }

    /// Reads whatever the compositor has queued without blocking.
    pub fn pump(&mut self) -> Result<()> {
        self.queue.flush()?;
        if let Some(guard) = self.queue.prepare_read() {
            if readable(self.fd()) {
                guard.read()?;
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        Ok(())
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.state.dirty)
    }

    pub fn show(&mut self) -> Result<()> {
        if self.state.mapped.is_some() {
            return Ok(());
        }
        let qh = self.queue.handle();
        let surface = self.state.compositor.create_surface(&qh, ());

        // Empty input region: pointer events pass through to what is below.
        let region = self.state.compositor.create_region(&qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();

        let layer = self.state.layer_shell.get_layer_surface(
            &surface,
            None,
            Layer::Overlay,
            "duskr-osd".into(),
            &qh,
            (),
        );
        layer.set_size(self.placement.width, self.placement.height);
        layer.set_anchor(anchor_for(self.placement.position));
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Float above panels rather than below them.
        layer.set_exclusive_zone(-1);
        let margin = self.placement.margin as i32;
        let (top, right, bottom, left) = margins_for(self.placement.position, margin);
        layer.set_margin(top, right, bottom, left);
        surface.commit();

        self.state.mapped = Some(Mapped {
            surface,
            layer,
            pool: Pool::new(&self.state.shm, &qh, 1, 1)?,
            size: (0, 0),
            configured: false,
            next_slot: 0,
        });
        self.state.closed = false;
        self.queue.flush()?;
        Ok(())
    }

    pub fn hide(&mut self) {
        self.state.mapped = None;
        self.state.entered.clear();
        self.state.released = [true; SLOTS];
        let _ = self.queue.flush();
    }

    /// False when no buffer is free, meaning the frame is skipped.
    pub fn present(&mut self, canvas: &Canvas) -> Result<bool> {
        let qh = self.queue.handle();
        let Some(mapped) = &mut self.state.mapped else {
            return Ok(false);
        };
        if !mapped.configured {
            return Ok(false);
        }

        if mapped.size != (canvas.width, canvas.height) {
            mapped.pool = Pool::new(&self.state.shm, &qh, canvas.width, canvas.height)?;
            mapped.size = (canvas.width, canvas.height);
            self.state.released = [true; SLOTS];
            mapped.next_slot = 0;
        }

        let slot = (0..SLOTS)
            .map(|offset| (mapped.next_slot + offset) % SLOTS)
            .find(|slot| self.state.released[*slot]);
        let Some(slot) = slot else {
            return Ok(false);
        };

        mapped.pool.write(slot, canvas.bytes());
        self.state.released[slot] = false;
        mapped.next_slot = (slot + 1) % SLOTS;

        mapped.surface.set_buffer_scale(canvas.scale as i32);
        mapped
            .surface
            .attach(Some(&mapped.pool.buffers[slot]), 0, 0);
        mapped
            .surface
            .damage_buffer(0, 0, canvas.width as i32, canvas.height as i32);
        mapped.surface.commit();
        self.queue.flush()?;
        Ok(true)
    }
}

fn anchor_for(position: OsdPosition) -> Anchor {
    match position {
        OsdPosition::Top => Anchor::Top,
        OsdPosition::Bottom => Anchor::Bottom,
        OsdPosition::TopLeft => Anchor::Top | Anchor::Left,
        OsdPosition::TopRight => Anchor::Top | Anchor::Right,
        OsdPosition::BottomLeft => Anchor::Bottom | Anchor::Left,
        OsdPosition::BottomRight => Anchor::Bottom | Anchor::Right,
        OsdPosition::Center => Anchor::empty(),
    }
}

/// Applied only on the anchored edges.
fn margins_for(position: OsdPosition, margin: i32) -> (i32, i32, i32, i32) {
    let anchor = anchor_for(position);
    (
        if anchor.contains(Anchor::Top) {
            margin
        } else {
            0
        },
        if anchor.contains(Anchor::Right) {
            margin
        } else {
            0
        },
        if anchor.contains(Anchor::Bottom) {
            margin
        } else {
            0
        },
        if anchor.contains(Anchor::Left) {
            margin
        } else {
            0
        },
    )
}

fn readable(fd: std::os::fd::RawFd) -> bool {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut poll, 1, 0) > 0 }
}
