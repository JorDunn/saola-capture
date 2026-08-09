//! Video capture: niri's `org.gnome.Mutter.ScreenCast` session plus the
//! PipeWire stream that carries the frames (PLAN.md Stage 10; the
//! [`CaptureBackend`](super::CaptureBackend) module's "video via niri's
//! `org.gnome.Mutter.ScreenCast` v4 + a PipeWire stream on a dedicated
//! thread" half).
//!
//! Everything here is downstream of `docs/CAPTURE-RESEARCH.md` §2 and its
//! decision D4. **Do not re-derive any of it from theory** — the numbers and
//! failure shapes below were measured against this exact compositor with a C
//! probe (`docs/research/2026-08-08-stage2/pwprobe.c`) whose transcripts are
//! in the same directory.
//!
//! # The shape of one recording, end to end
//!
//! ```text
//!   zbus (tokio)                          dedicated OS thread (pw main loop)
//!   ────────────                          ─────────────────────────────────
//!   CreateSession        ──▶ o /…/Session
//!   RecordMonitor(name)  ──▶ o /…/Stream
//!   subscribe Closed + PipeWireStreamAdded   ← both BEFORE Start (see below)
//!   Start                ──▶ ()
//!   PipeWireStreamAdded(node id) ─────────▶ PipeWireStream::connect(node)
//!                                             pw_stream_connect(EnumFormat)
//!                                             param_changed → Buffers reply
//!                                             process → mmap dmabuf, copy
//!                            CastControl ◀──   Negotiated / Error / Ended
//!                            VideoFrame  ◀──   bounded, try_send, drop+count
//!   Session.Stop         ◀── after the pw thread is joined
//! ```
//!
//! # Five things that are *not* negotiable, with their evidence
//!
//! 1. **dmabuf, modifier `LINEAR`, mmap the fd. There is no shm path.**
//!    CAPTURE-RESEARCH §2.1 tried shm two ways and niri refused both: with
//!    no `modifier` property at all the format negotiation itself dies
//!    (`no more input formats`), and with the modifier advertised but
//!    `dataType = MemFd|MemPtr` the *buffer allocation* dies
//!    (`error alloc buffers: Invalid argument`). This module therefore
//!    offers exactly one format and treats a non-dmabuf buffer as a
//!    first-class error ([`CastControl::Error`]), never as a case to
//!    silently handle. §2.4: "negotiation produced no format" must be
//!    user-visible.
//! 2. **`PW_STREAM_FLAG_MAP_BUFFERS` does not map dmabufs.** Every probe
//!    frame had `data = (nil)` with the flag set. [`FrameMapping`] does the
//!    `mmap` itself.
//! 3. **`chunk->stride` is the authority for the row pitch, and it is not
//!    `width * 4`.** On the window cast the negotiated size was 2507×1457
//!    but the stride was 10240 (= 2560 × 4, the *output* pitch), because the
//!    buffer is allocated at output pitch. Deriving the stride from the
//!    width shears every frame. [`copy_packed_rows`] is the one place this
//!    is applied, and it is unit-tested.
//! 4. **`maxsize` and `chunk->size` are dummies on the dmabuf path** (both
//!    reported `1`). The mapping length comes from `lseek(fd, 0, SEEK_END)`,
//!    and the fd is *larger* than `stride × height` (allocator padding past
//!    the last row) — 14 921 728 vs 14 919 680 in the probe.
//! 5. **The framerate negotiates to `0/1` (variable) with `maxFramerate
//!    60000/1000`.** Frames are damage-driven: the probe's window cast
//!    delivered **one frame in six seconds** from an idle terminal. Nothing
//!    here may treat frame silence as failure, and Stage 11's encoder gets
//!    its timestamps from ffmpeg's `-use_wallclock_as_timestamps 1`
//!    (CAPTURE-RESEARCH §4.2/D6), *not* from anything in [`VideoFrame`].
//!
//! # Why a dedicated OS thread (teaching note)
//!
//! `pw_main_loop_run` is a blocking, callback-driven event loop written in
//! C. It is not a future, cannot be polled, and owns the thread it runs on
//! until something calls `pw_main_loop_quit`. Meanwhile CLAUDE.md's
//! **one-runtime rule** says this process runs exactly one async runtime
//! (tokio, shared with zbus and iced) and never a second — so the PipeWire
//! loop cannot become "another executor". The sanctioned resolution, spelled
//! out in CLAUDE.md ("The **PipeWire main loop is the one sanctioned extra
//! thread**"), is what [`PipeWireStream::connect`] does: spawn one plain
//! `std::thread`, keep every PipeWire object inside it, and let *channels*
//! be the only thing that crosses the boundary.
//!
//! That "keep every object inside it" is a hard requirement, not tidiness:
//! `MainLoopRc`, `ContextRc`, `CoreRc` and `StreamBox` are all `Rc`-based
//! and therefore **not `Send`**. They are constructed inside
//! [`stream_thread`] and never leave it. The two things that *do* cross are
//! both `Send` by construction:
//!
//! - **outward** (pw thread → owner): two `std::sync::mpsc` channels.
//! - **inward** (owner → pw thread): a `pipewire::channel::Sender`, whose
//!   receiving half is attached to the pw loop as an event source. This is
//!   the *only* correct way to poke a running pw main loop from another
//!   thread; calling `quit()` on it directly from outside would be a data
//!   race on a non-`Sync` object.
//!
//! # Why `std::sync::mpsc` here, and not `iced::futures::channel::mpsc`
//!
//! CLAUDE.md records the daemon-side precedent (`dbus_worker_stream` uses
//! `iced::futures::channel::mpsc` because iced already re-exports `futures`,
//! so it costs no new crate) and explicitly leaves this stage free to
//! decide: "Stage 10's PipeWire-thread-to-daemon bridge is the next place
//! this pattern almost certainly gets reused (or `tokio::sync`, if the
//! pipewire thread isn't itself async — decide there)."
//!
//! Decided: **`std::sync::mpsc`**, because the pipewire thread is not async.
//! It is a C event loop calling synchronous callbacks; there is no executor
//! there to `.await` on and no `Waker` to wake. A futures channel's
//! `try_send` would work, but its whole reason to exist — an async receiver
//! — is unused on both ends, since Stage 11's consumer is a blocking write
//! into ffmpeg's stdin. `std::sync::mpsc::sync_channel(N)` is bounded, has
//! exactly the [`SyncSender::try_send`](std::sync::mpsc::SyncSender::try_send)
//! semantics PLAN.md's backpressure rule demands ("bounded channel, drop
//! frames and log when full"), and costs zero new crates or features.
//!
//! **Two channels, not one**, and that split is load-bearing: frames go
//! through the bounded one and may be dropped; control messages
//! ([`CastControl`] — the negotiated format, a fatal error, end-of-stream)
//! go through an unbounded one and never may be. Merging them would make
//! "the queue was briefly full" able to swallow the negotiated format.
//!
//! # Teardown ordering (binding — Stage 11 consumes this verbatim)
//!
//! **Normal stop:** stop the *consumer* first, the *producer* second.
//! [`PipeWireStream::stop`] (signal the pw loop → `pw_main_loop_quit` →
//! `pw_stream_disconnect` → join the thread) and only then
//! [`CastSession::close`] (`Session.Stop`). Doing it the other way round
//! makes the node vanish under a live consumer, which surfaces as a
//! spurious `StreamState::Error` and a scary log line for what was actually
//! a clean, user-requested stop.
//!
//! **Compositor-initiated stop:** the session dies on its own (output
//! unplugged, `niri msg action stop-cast`, or CAPTURE-RESEARCH §5.3's
//! "`Start` succeeds for a bogus window id and then the session
//! self-destructs"). The authority for this is the `Session.Closed` D-Bus
//! signal, subscribed **before** `Start` and retained afterwards so
//! [`CastSession::closed`] can be consulted at any time. **Never call
//! `Session.Stop` on a closed session** (D8) — [`CastSession::close`]
//! checks first.
//!
//! **Stream error:** the pw thread reports [`CastControl::Error`], then
//! quits its own loop and sends [`CastControl::Ended`]. The owner still
//! calls `stop()` (idempotent — the thread is already exiting) and then
//! `close()`.
//!
//! # `CastsChanged` awareness
//!
//! niri publishes its own view of every live cast over niri-ipc, which is a
//! useful independent cross-check of the two-protocol handshake above:
//! [`casts_snapshot`] reads it, [`cast_for_node`] finds ours by PipeWire
//! node id, and [`CastSummary`] is the flattened form the dry run logs.
//! `is_active` there means "a consumer is attached", not "a cast exists"
//! (CAPTURE-RESEARCH D11), so watching it flip to `true` after
//! [`PipeWireStream::connect`] is a genuine end-to-end confirmation that the
//! compositor sees *our* consumer.
//!
//! A **push** subscription (niri's `Event::CastsChanged` /
//! `Event::CastStopped` over `Request::EventStream`) is deliberately not
//! built here: the niri event socket is blocking, so consuming it needs its
//! own long-lived thread, and this stage already spends the one thread
//! CLAUDE.md sanctions. It also would not add authority — `Session.Closed`
//! is the signal that actually decides whether `Session.Stop` may be called,
//! and it arrives on the connection this module already owns. Stage 12
//! (tray + recorder state machine) is where a push subscription earns a
//! thread; the exact variants it wants are `niri_ipc::Event::CastsChanged {
//! casts }` and `niri_ipc::Event::CastStopped { stream_id }`.

use std::collections::HashMap;
use std::fmt;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iced::futures::{FutureExt, StreamExt};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

/// `DRM_FORMAT_MOD_LINEAR` — the one modifier this app asks for and the one
/// that needs no GBM/EGL import to read (CAPTURE-RESEARCH §2.2). Spelled
/// here rather than pulled from a DRM crate: it is a single, frozen,
/// well-known constant from `drm_fourcc.h`, and adding a dependency for one
/// `0` would fail this repo's dependency bar.
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// How long [`CastSession::open`] waits for `PipeWireStreamAdded` after
/// `Start` before giving up.
///
/// The probe saw the signal "~immediately after Start" (pre-plan SUMMARY),
/// so this is not a latency budget — it is the timeout that turns
/// CAPTURE-RESEARCH §5.3's "`Start` returns success for a bogus window id
/// and then the session self-destructs" into an error the user can read
/// instead of a hang.
const NODE_ID_TIMEOUT: Duration = Duration::from_secs(5);

/// Bound on the frame queue between the pw thread and its consumer.
///
/// Four frames of slack ≈ 66 ms at the 60 fps cap — enough to ride out a
/// scheduling hiccup or an ffmpeg write that briefly blocks, short enough
/// that the memory ceiling stays sane: the queue holds *packed copies*, so
/// the worst case is `4 × width × height × 4` bytes ≈ **64 MB** at
/// 2560×1600. Raising this trades latency and memory for stall tolerance;
/// it does not trade away frame drops, because a producer that is
/// permanently faster than the consumer overruns any finite queue.
const FRAME_QUEUE_CAPACITY: usize = 4;

/// `DMA_BUF_IOCTL_SYNC`, i.e. `_IOW('b', 0, struct dma_buf_sync)`.
///
/// Computed the way `<linux/dma-buf.h>`'s macros do:
/// `dir(_IOC_WRITE=1) << 30 | size(8) << 16 | type('b'=0x62) << 8 | nr(0)`
/// = `0x4008_6200`. Written as a literal with the derivation spelled out
/// rather than reconstructing `_IOC` in Rust, and cast through
/// [`libc::Ioctl`] because that alias is `c_ulong` on glibc and `c_int` on
/// musl.
const DMA_BUF_IOCTL_SYNC: libc::Ioctl = 0x4008_6200 as libc::Ioctl;
const DMA_BUF_SYNC_READ: u64 = 1 << 0;
const DMA_BUF_SYNC_START: u64 = 0 << 2;
const DMA_BUF_SYNC_END: u64 = 1 << 2;

/// `struct dma_buf_sync` from `<linux/dma-buf.h>` — one `__u64 flags`.
/// Declared locally because `libc` does not bind the DMA-BUF uapi.
#[repr(C)]
struct DmaBufSync {
    flags: u64,
}

// ---------------------------------------------------------------------------
// What to cast, and how the cursor is treated
// ---------------------------------------------------------------------------

/// What a cast points at.
///
/// There is deliberately no `Region` variant: **niri's session interface has
/// no `RecordArea`** (verified by introspection — see CAPTURE-RESEARCH §2
/// and PLAN.md's Architecture note). Region recording is a *monitor* cast
/// cropped later in ffmpeg's filter chain (D8), so it is a
/// [`Self::Monitor`] plus a crop rectangle that this module never sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CastTarget {
    /// `RecordMonitor(connector)` — `connector` is the output *name*
    /// (`"eDP-1"`), the same string [`OutputInfo::name`](super::OutputInfo)
    /// carries.
    Monitor { connector: String },
    /// `RecordWindow({"window-id": <t>})` — a niri-ipc window id. The id
    /// spaces are unified (CAPTURE-RESEARCH §5.1), so this is the same
    /// number `shot --window --window-id` takes.
    Window { id: u64 },
}

impl fmt::Display for CastTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CastTarget::Monitor { connector } => write!(f, "monitor {connector}"),
            CastTarget::Window { id } => write!(f, "window {id}"),
        }
    }
}

/// Mutter's `cursor-mode` enum, as far as this app uses it.
///
/// **Not the xdg-desktop-portal bitmask** — a live probe rejected `3` and
/// `4` with "expected variant index 0 <= i < 3" (pre-plan SUMMARY §3), which
/// is exactly the mistake this named type exists to prevent. If you are
/// tempted to add a value, the wire enum is `0 = hidden`, `1 = embedded`,
/// `2 = metadata`, and nothing else.
///
/// **Mutter's third value, `2` (metadata), is deliberately absent**: it
/// delivers the pointer as buffer metadata instead of compositing it in,
/// which would mean drawing the cursor ourselves into every frame. The
/// `cursor`/`--cursor` knob is a boolean, so v0.1 has no way to ask for it
/// and no code that could honour it — a variant nothing can construct would
/// only be a trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    Hidden = 0,
    Embedded = 1,
}

impl CursorMode {
    /// The `--cursor`/`cursor` config knob's two states.
    pub fn from_cursor_option(cursor: bool) -> Self {
        if cursor {
            CursorMode::Embedded
        } else {
            CursorMode::Hidden
        }
    }

    fn as_u32(self) -> u32 {
        self as u32
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong between "start recording" and "frames are
/// arriving".
///
/// Deliberately its own type rather than a new arm of
/// [`CaptureError`](super::CaptureError): that one is shaped around
/// `zwlr_screencopy_v1` (missing globals, shm formats, `y_invert`), and none
/// of its vocabulary applies to a D-Bus session plus a PipeWire node.
#[derive(Debug)]
pub enum CastError {
    /// The session bus itself is unreachable.
    Bus(String),
    /// `org.gnome.Mutter.ScreenCast` is not being served, or a method call
    /// on it failed. Carries the method name so the message says *which*
    /// hop broke.
    ScreenCast { method: &'static str, err: String },
    /// `Start` succeeded but no `PipeWireStreamAdded` arrived within
    /// [`NODE_ID_TIMEOUT`].
    NoNodeId,
    /// The compositor closed the session before it produced a node — the
    /// documented shape of a bad `RecordWindow` id (CAPTURE-RESEARCH §5.3).
    ClosedEarly,
    /// PipeWire itself refused: no daemon, no permission, or
    /// `pw_stream_connect` failed.
    PipeWire(String),
    /// The thread that owns the PipeWire loop could not be spawned, or died
    /// without reporting anything.
    Thread(String),
}

impl fmt::Display for CastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CastError::Bus(err) => write!(f, "could not reach the session bus: {err}"),
            CastError::ScreenCast { method, err } => write!(
                f,
                "org.gnome.Mutter.ScreenCast.{method} failed: {err} — is niri running, and does \
                 `busctl --user introspect org.gnome.Mutter.ScreenCast /org/gnome/Mutter/ScreenCast` \
                 answer?"
            ),
            CastError::NoNodeId => write!(
                f,
                "the compositor started the screencast session but never announced a PipeWire \
                 node — nothing to record from"
            ),
            CastError::ClosedEarly => write!(
                f,
                "the compositor closed the screencast session immediately — the recording target \
                 is probably gone (a window that closed, or an output that was unplugged)"
            ),
            CastError::PipeWire(err) => write!(
                f,
                "could not attach to the PipeWire stream: {err} — is the `pipewire` service \
                 running (`systemctl --user status pipewire`)?"
            ),
            CastError::Thread(err) => write!(f, "the PipeWire capture thread failed: {err}"),
        }
    }
}

impl std::error::Error for CastError {}

// ---------------------------------------------------------------------------
// D-Bus: org.gnome.Mutter.ScreenCast
// ---------------------------------------------------------------------------
//
// Teaching note on `#[zbus::proxy]`: this generates a *client*, the mirror
// image of `dbus.rs`'s `#[zbus::interface]` server. Each trait method
// becomes an async method on a generated `…Proxy` struct whose name is the
// trait's plus `Proxy`; each `#[zbus(signal)]` method additionally generates
// `receive_<name>()`, returning a stream of that signal. The wire names are
// derived by PascalCasing the Rust names — spelled out explicitly below
// wherever the derivation is not obvious (`PipeWireStreamAdded` is the one
// that matters).
//
// These traits are private: nothing outside this module should be able to
// drive a Mutter session directly, since the lifecycle rules above are the
// whole point of `CastSession`.

#[zbus::proxy(
    interface = "org.gnome.Mutter.ScreenCast",
    default_service = "org.gnome.Mutter.ScreenCast",
    default_path = "/org/gnome/Mutter/ScreenCast"
)]
trait ScreenCast {
    /// `CreateSession(a{sv}) -> o`. niri accepts an empty properties map.
    fn create_session(&self, properties: HashMap<&str, Value<'_>>)
        -> zbus::Result<OwnedObjectPath>;

    /// `Version` — `4` on niri 26.04. Read once at open purely so the log
    /// records what we negotiated against.
    #[zbus(property)]
    fn version(&self) -> zbus::Result<i32>;
}

#[zbus::proxy(
    interface = "org.gnome.Mutter.ScreenCast.Session",
    default_service = "org.gnome.Mutter.ScreenCast"
)]
trait ScreenCastSession {
    /// `RecordMonitor(s, a{sv}) -> o`.
    fn record_monitor(
        &self,
        connector: &str,
        properties: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;

    /// `RecordWindow(a{sv}) -> o` — note there is **no** positional
    /// argument; the window id rides in the properties map as `window-id`
    /// (`t`). A bogus id is accepted here and only fails later, which is why
    /// [`CastSession::open`] watches `Closed`.
    fn record_window(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;

    fn start(&self) -> zbus::Result<()>;

    fn stop(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn closed(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.gnome.Mutter.ScreenCast.Stream",
    default_service = "org.gnome.Mutter.ScreenCast"
)]
trait ScreenCastStream {
    /// `Parameters a{sv}` — `position (ii)` and `size (ii)`, both in
    /// **logical** pixels (1706×1066 on Jordan's 2560×1600 @ 1.5 output).
    /// The SPA format, by contrast, is **physical**. Keeping both is what
    /// lets Stage 12's region crop convert between them without guessing.
    #[zbus(property)]
    fn parameters(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    #[zbus(signal, name = "PipeWireStreamAdded")]
    fn pipe_wire_stream_added(&self, node_id: u32) -> zbus::Result<()>;
}

/// `Stream.Parameters`, decoded. Logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParameters {
    pub position: (i32, i32),
    pub size: (i32, i32),
}

/// A live Mutter.ScreenCast session with exactly one stream, already
/// `Start`ed, whose PipeWire node id is known.
///
/// Owns the `Closed` signal stream for its whole life so
/// [`Self::closed`] can answer "did the compositor kill this?" at any point
/// without a round trip — which is what makes the "never `Stop` a closed
/// session" rule (D8) enforceable rather than aspirational.
pub struct CastSession {
    session: ScreenCastSessionProxy<'static>,
    closed_signals: ClosedStream,
    node_id: u32,
    parameters: Option<StreamParameters>,
    target: CastTarget,
    closed: bool,
}

impl CastSession {
    /// `CreateSession` → `Record*` → subscribe → `Start` → node id.
    ///
    /// **Subscription order is load-bearing.** Both signal streams are
    /// opened *before* `Start`, because `PipeWireStreamAdded` fires
    /// "~immediately after Start" (pre-plan probe) — subscribing afterwards
    /// is a race the fast path loses.
    pub async fn open(
        connection: &Connection,
        target: &CastTarget,
        cursor: CursorMode,
    ) -> Result<Self, CastError> {
        let screencast =
            ScreenCastProxy::new(connection)
                .await
                .map_err(|err| CastError::ScreenCast {
                    method: "CreateSession",
                    err: err.to_string(),
                })?;

        // Informational only; a failure here is not fatal (the methods below
        // are the real capability test).
        match screencast.version().await {
            Ok(version) => {
                eprintln!("saola-capture: screencast: Mutter.ScreenCast version {version}")
            }
            Err(err) => eprintln!("saola-capture: screencast: could not read Version: {err}"),
        }

        let session_path = screencast
            .create_session(HashMap::new())
            .await
            .map_err(|err| CastError::ScreenCast {
                method: "CreateSession",
                err: err.to_string(),
            })?;

        let session = ScreenCastSessionProxy::builder(connection)
            .path(session_path.clone())
            .map_err(|err| CastError::ScreenCast {
                method: "CreateSession",
                err: err.to_string(),
            })?
            .build()
            .await
            .map_err(|err| CastError::ScreenCast {
                method: "CreateSession",
                err: err.to_string(),
            })?;

        // Subscribe to Closed before anything can close.
        let mut closed_signals =
            session
                .receive_closed()
                .await
                .map_err(|err| CastError::ScreenCast {
                    method: "Closed",
                    err: err.to_string(),
                })?;

        let mut properties: HashMap<&str, Value<'_>> = HashMap::new();
        properties.insert("cursor-mode", Value::U32(cursor.as_u32()));

        let (method, stream_path) = match target {
            CastTarget::Monitor { connector } => (
                "RecordMonitor",
                session.record_monitor(connector, properties).await,
            ),
            CastTarget::Window { id } => {
                properties.insert("window-id", Value::U64(*id));
                ("RecordWindow", session.record_window(properties).await)
            }
        };
        let stream_path = stream_path.map_err(|err| CastError::ScreenCast {
            method,
            err: err.to_string(),
        })?;

        let stream = ScreenCastStreamProxy::builder(connection)
            .path(stream_path.clone())
            .map_err(|err| CastError::ScreenCast {
                method,
                err: err.to_string(),
            })?
            .build()
            .await
            .map_err(|err| CastError::ScreenCast {
                method,
                err: err.to_string(),
            })?;

        let mut added = stream
            .receive_pipe_wire_stream_added()
            .await
            .map_err(|err| CastError::ScreenCast {
                method: "PipeWireStreamAdded",
                err: err.to_string(),
            })?;

        session.start().await.map_err(|err| CastError::ScreenCast {
            method: "Start",
            err: err.to_string(),
        })?;

        // Three-way race. `Closed` losing to the timeout is not the same
        // error as no signal at all, so the two are distinguished: a closed
        // session must never be `Stop`ped, a merely-silent one still should.
        let node_id = tokio::select! {
            signal = added.next() => match signal {
                Some(signal) => match signal.args() {
                    Ok(args) => *args.node_id(),
                    Err(err) => {
                        let _ = session.stop().await;
                        return Err(CastError::ScreenCast {
                            method: "PipeWireStreamAdded",
                            err: err.to_string(),
                        });
                    }
                },
                None => {
                    let _ = session.stop().await;
                    return Err(CastError::NoNodeId);
                }
            },
            _ = closed_signals.next() => return Err(CastError::ClosedEarly),
            _ = tokio::time::sleep(NODE_ID_TIMEOUT) => {
                let _ = session.stop().await;
                return Err(CastError::NoNodeId);
            }
        };

        // Best-effort: an undecodable Parameters map costs nothing but a
        // less informative log, so it warns rather than failing a recording
        // that is otherwise ready to go.
        let parameters = match stream.parameters().await {
            Ok(map) => decode_stream_parameters(&map),
            Err(err) => {
                eprintln!("saola-capture: screencast: could not read Stream.Parameters: {err}");
                None
            }
        };

        eprintln!(
            "saola-capture: screencast: {target} → session {} stream {} node {node_id}{}",
            session_path.as_str(),
            stream_path.as_str(),
            match parameters {
                Some(p) => format!(
                    " (logical {}x{} at {},{})",
                    p.size.0, p.size.1, p.position.0, p.position.1
                ),
                None => String::new(),
            }
        );

        Ok(CastSession {
            session,
            closed_signals,
            node_id,
            parameters,
            target: target.clone(),
            closed: false,
        })
    }

    /// The PipeWire node [`PipeWireStream::connect`] should attach to.
    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    /// `Stream.Parameters`, in logical pixels, if it decoded.
    pub fn parameters(&self) -> Option<StreamParameters> {
        self.parameters
    }

    pub fn target(&self) -> &CastTarget {
        &self.target
    }

    /// Has the compositor closed this session behind our back?
    ///
    /// Non-blocking: drains whatever `Closed` signals are already queued and
    /// latches the answer. Call this before anything that would touch the
    /// session — above all `Stop`, which D8 says must never be sent to a
    /// closed session.
    pub fn closed(&mut self) -> bool {
        // `now_or_never` polls the stream exactly once and gives up rather
        // than waiting — the whole point being that this is a poll, not an
        // await, so a recorder state machine can consult it from anywhere.
        while let Some(item) = self.closed_signals.next().now_or_never() {
            match item {
                // A real `Closed` signal.
                Some(_) => self.closed = true,
                // The stream itself ended (connection gone): the session is
                // certainly not usable any more either.
                None => {
                    self.closed = true;
                    break;
                }
            }
        }
        self.closed
    }

    /// `Session.Stop`, unless the compositor already closed it.
    ///
    /// Consumes `self`: a stopped session has no valid operations left, and
    /// making that a type-level fact is cheaper than remembering it.
    pub async fn close(mut self) {
        if self.closed() {
            eprintln!(
                "saola-capture: screencast: session was already closed by the compositor — not \
                 calling Stop"
            );
            return;
        }
        if let Err(err) = self.session.stop().await {
            eprintln!("saola-capture: screencast: Session.Stop failed: {err}");
        }
    }
}

/// Pulls `position`/`size` out of `Stream.Parameters`.
///
/// Both are D-Bus `(ii)` structures. Written against
/// [`zbus::zvariant::Structure`] by hand rather than `TryFrom<OwnedValue>`
/// for a tuple, because a missing or differently-typed key must degrade to
/// `None` (a less informative log) rather than fail a recording.
fn decode_stream_parameters(map: &HashMap<String, OwnedValue>) -> Option<StreamParameters> {
    Some(StreamParameters {
        position: decode_int_pair(map.get("position")?)?,
        size: decode_int_pair(map.get("size")?)?,
    })
}

fn decode_int_pair(value: &OwnedValue) -> Option<(i32, i32)> {
    let structure: &zbus::zvariant::Structure<'_> = value.downcast_ref().ok()?;
    let fields = structure.fields();
    let first = i32::try_from(fields.first()?).ok()?;
    let second = i32::try_from(fields.get(1)?).ok()?;
    Some((first, second))
}

// ---------------------------------------------------------------------------
// niri's own view of the cast
// ---------------------------------------------------------------------------

/// One live cast, as niri reports it — the flattened, `Display`-able form of
/// [`niri_ipc::Cast`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastSummary {
    pub stream_id: u64,
    pub session_id: u64,
    pub target: String,
    /// **"a consumer is attached", not "a cast exists"** (CAPTURE-RESEARCH
    /// D11). Watching this flip to `true` is how the dry run proves the
    /// PipeWire half actually connected.
    pub is_active: bool,
}

impl fmt::Display for CastSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stream {} (session {}) → {}, consumer attached: {}",
            self.stream_id, self.session_id, self.target, self.is_active
        )
    }
}

/// Ask niri for every cast it currently has. `None` when its IPC socket
/// can't be reached — this is a cross-check, never a dependency, so every
/// caller treats absence as "no extra information", not as an error.
pub fn casts_snapshot() -> Option<Vec<niri_ipc::Cast>> {
    let mut socket = niri_ipc::socket::Socket::connect().ok()?;
    match socket.send(niri_ipc::Request::Casts).ok()?.ok()? {
        niri_ipc::Response::Casts(casts) => Some(casts),
        _ => None,
    }
}

/// Find the cast that owns `node_id`.
///
/// Pure so it can be unit-tested without a compositor. A cast that niri has
/// created but not yet given a node reports `pw_node_id: None` (its own
/// docs say so), which is why the comparison is against `Some(node_id)`
/// rather than an unwrap.
pub fn cast_for_node(casts: &[niri_ipc::Cast], node_id: u32) -> Option<&niri_ipc::Cast> {
    casts.iter().find(|cast| cast.pw_node_id == Some(node_id))
}

/// Flatten a [`niri_ipc::Cast`] for logging.
pub fn summarize_cast(cast: &niri_ipc::Cast) -> CastSummary {
    let target = match &cast.target {
        niri_ipc::CastTarget::Nothing {} => "no target".to_string(),
        niri_ipc::CastTarget::Output { name } => format!("output {name}"),
        niri_ipc::CastTarget::Window { id } => format!("window {id}"),
    };
    CastSummary {
        stream_id: cast.stream_id,
        session_id: cast.session_id,
        target,
        is_active: cast.is_active,
    }
}

// ---------------------------------------------------------------------------
// The PipeWire side
// ---------------------------------------------------------------------------

/// The SPA format the node and this consumer agreed on.
///
/// Everything Stage 11 needs to build ffmpeg's `-video_size`/`-pixel_format`
/// arguments, and nothing it doesn't. `width`/`height` are **physical**
/// pixels (2560×1600 on Jordan's laptop, not the 1706×1066
/// [`StreamParameters`] reports).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedFormat {
    pub width: u32,
    pub height: u32,
    /// Always `LINEAR` (0) in practice — this offer contains no other
    /// option — but read back from the negotiated pod rather than assumed,
    /// so a compositor that somehow fixated something else is visible in the
    /// log instead of silently producing garbage.
    pub modifier: u64,
    /// `(num, denom)`. Negotiates to `0/1` — variable — against niri.
    pub framerate: (u32, u32),
    /// `(num, denom)`. `60000/1000` against niri.
    pub max_framerate: (u32, u32),
}

impl NegotiatedFormat {
    /// The format's dimensions rounded **down** to even, which is what
    /// `hevc_vaapi` needs (CAPTURE-RESEARCH §3.6: it silently emits a
    /// 2508×1458 stream for a 2507×1457 input). Here rather than in Stage
    /// 11's encoder because it is a property of the *negotiated format*, and
    /// because the crop it implies has to agree with the row copy that
    /// already happened.
    pub fn even_dimensions(&self) -> (u32, u32) {
        (self.width & !1, self.height & !1)
    }

    /// Bytes in one packed frame of this format: `width * 4 * height`, in
    /// `usize`, saturating rather than overflowing.
    pub fn packed_len(&self) -> usize {
        (self.width as usize)
            .saturating_mul(4)
            .saturating_mul(self.height as usize)
    }
}

impl fmt::Display for NegotiatedFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BGRx {}x{} modifier 0x{:x} framerate {}/{} maxFramerate {}/{}",
            self.width,
            self.height,
            self.modifier,
            self.framerate.0,
            self.framerate.1,
            self.max_framerate.0,
            self.max_framerate.1,
        )
    }
}

/// One captured frame, already copied out of the compositor's dmabuf and
/// **packed** (`width * 4` bytes per row, no padding).
///
/// Packing here, once, is deliberate: it is the single place
/// CAPTURE-RESEARCH §2.3's stride gotcha has to be right, and it hands Stage
/// 11 exactly the byte layout ffmpeg's `-f rawvideo -pixel_format bgr0`
/// wants, so the encoder never has to know a stride existed.
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, B G R X per pixel (`bgr0` to ffmpeg).
    pub bytes: Vec<u8>,
    /// Frames this stream has delivered before this one, counting from 0.
    /// Gaps are impossible — drops are counted separately
    /// ([`PipeWireStream::dropped_frames`]) precisely so this stays a clean
    /// "how many did we hand on".
    pub sequence: u64,
    /// When the pw thread finished copying it.
    ///
    /// **Not a presentation timestamp.** D6 pins ffmpeg's
    /// `-use_wallclock_as_timestamps 1`, so the PTS is taken at the moment
    /// bytes are written to the encoder's stdin. This field is for cadence
    /// diagnostics and for Stage 12's "is the recording still alive?"
    /// question, and must not become a muxing input.
    pub captured_at: Instant,
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Hand-written so a 16 MB pixel buffer never lands in a log line.
        f.debug_struct("VideoFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes.len())
            .field("sequence", &self.sequence)
            .finish()
    }
}

/// Out-of-band news from the pw thread. Never dropped (see the module doc's
/// two-channel note).
#[derive(Debug)]
pub enum CastControl {
    /// Negotiation succeeded. Always the first message on a healthy stream.
    Negotiated(NegotiatedFormat),
    /// A fatal, user-visible problem. CAPTURE-RESEARCH §2.4: "negotiation
    /// produced no format" and its relatives are first-class error paths,
    /// not silent degradations.
    Error(String),
    /// The pw loop has left `run()`. Always the last message, sent on every
    /// path including a clean stop.
    Ended,
}

/// Message the owner sends *into* the pw main loop. One variant today; a
/// named type anyway, so the next one (Stage 12's pause?) is an added
/// variant rather than a changed channel type.
enum LoopCommand {
    Stop,
}

/// A running PipeWire consumer on its own thread.
///
/// Dropping this stops and joins the thread, so a `?` anywhere in a caller
/// cannot leak the thread or leave a consumer attached to a node the
/// session is about to destroy.
pub struct PipeWireStream {
    control: Receiver<CastControl>,
    frames: Receiver<VideoFrame>,
    stop: pipewire::channel::Sender<LoopCommand>,
    thread: Option<JoinHandle<()>>,
    dropped: Arc<AtomicU64>,
}

impl PipeWireStream {
    /// Spawn the thread and attach to `node_id`.
    ///
    /// Returns as soon as the thread is spawned — **not** once the format is
    /// negotiated. The caller learns that from the first
    /// [`CastControl::Negotiated`] on [`Self::control`], which is also where
    /// a negotiation failure surfaces. Splitting it that way is what keeps
    /// the damage-driven reality of §2.3 gotcha 4 ("one frame in six
    /// seconds") from turning into a blocking constructor.
    pub fn connect(node_id: u32) -> Result<Self, CastError> {
        let (control_tx, control) = std::sync::mpsc::channel::<CastControl>();
        let (frames_tx, frames) = std::sync::mpsc::sync_channel::<VideoFrame>(FRAME_QUEUE_CAPACITY);
        let (stop, stop_rx) = pipewire::channel::channel::<LoopCommand>();
        let dropped = Arc::new(AtomicU64::new(0));

        let thread = std::thread::Builder::new()
            .name("saola-pipewire".to_string())
            .spawn({
                let dropped = Arc::clone(&dropped);
                move || stream_thread(node_id, control_tx, frames_tx, dropped, stop_rx)
            })
            .map_err(|err| CastError::Thread(err.to_string()))?;

        Ok(PipeWireStream {
            control,
            frames,
            stop,
            thread: Some(thread),
            dropped,
        })
    }

    /// The never-dropped control channel.
    pub fn control(&self) -> &Receiver<CastControl> {
        &self.control
    }

    /// The bounded frame channel.
    pub fn frames(&self) -> &Receiver<VideoFrame> {
        &self.frames
    }

    /// How many frames the pw thread had to throw away because this queue
    /// was full. PLAN.md's backpressure contract in one number.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stop the loop and join the thread. Idempotent, and the same code path
    /// [`Drop`] takes.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        // `send` returns the message back on failure, which here only means
        // the loop already exited — exactly the case where there is nothing
        // to do.
        let _ = self.stop.send(LoopCommand::Stop);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                eprintln!("saola-capture: screencast: the PipeWire thread panicked while stopping");
            }
        }
    }
}

impl Drop for PipeWireStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------
// The pw thread itself
// ---------------------------------------------------------------------------

/// State the PipeWire callbacks share. Lives entirely inside the pw thread,
/// handed to the callbacks as pipewire-rs's "user data".
struct StreamState {
    control: std::sync::mpsc::Sender<CastControl>,
    frames: SyncSender<VideoFrame>,
    dropped: Arc<AtomicU64>,
    format: Option<NegotiatedFormat>,
    /// dmabuf fd → its mapping, so a 16 MB `mmap`/`munmap` pair isn't paid
    /// per frame at 60 Hz. PipeWire reuses a small pool of buffers (2–16
    /// here) with stable fds for the pool's lifetime, so this stays tiny.
    mappings: HashMap<RawFd, FrameMapping>,
    sequence: u64,
    /// Latches after the first fatal error so a broken stream reports once
    /// rather than once per frame.
    failed: bool,
}

impl StreamState {
    fn fail(&mut self, message: String) {
        if self.failed {
            return;
        }
        self.failed = true;
        let _ = self.control.send(CastControl::Error(message));
    }
}

/// The thread body. Every PipeWire object is created, used and destroyed
/// here; none of them is `Send` and none of them escapes.
fn stream_thread(
    node_id: u32,
    control: std::sync::mpsc::Sender<CastControl>,
    frames: SyncSender<VideoFrame>,
    dropped: Arc<AtomicU64>,
    stop_rx: pipewire::channel::Receiver<LoopCommand>,
) {
    // Every early return has to announce the end, or a consumer blocking on
    // `control` waits forever. One helper, used on every path.
    let finish = |control: &std::sync::mpsc::Sender<CastControl>| {
        let _ = control.send(CastControl::Ended);
    };

    // `MainLoopRc::new` calls `pw_init` itself (verified in pipewire-rs's
    // own `MainLoopBox::new`), so there is no separate init step to get
    // wrong or to guard with a `Once`.
    let main_loop = match pipewire::main_loop::MainLoopRc::new(None) {
        Ok(loop_) => loop_,
        Err(err) => {
            let _ = control.send(CastControl::Error(format!(
                "could not create a PipeWire main loop: {err}"
            )));
            finish(&control);
            return;
        }
    };

    let context = match pipewire::context::ContextRc::new(&main_loop, None) {
        Ok(context) => context,
        Err(err) => {
            let _ = control.send(CastControl::Error(format!(
                "could not create a PipeWire context: {err}"
            )));
            finish(&control);
            return;
        }
    };

    let core = match context.connect_rc(None) {
        Ok(core) => core,
        Err(err) => {
            let _ = control.send(CastControl::Error(format!(
                "could not connect to the PipeWire daemon: {err}"
            )));
            finish(&control);
            return;
        }
    };

    // Same three properties the Stage 2 probe used, and the same ones every
    // screencast consumer sets: they are how PipeWire's session manager
    // classifies the node.
    let properties = pipewire::properties::properties! {
        *pipewire::keys::MEDIA_TYPE => "Video",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Screen",
    };

    let stream = match pipewire::stream::StreamBox::new(&core, "saola-capture", properties) {
        Ok(stream) => stream,
        Err(err) => {
            let _ = control.send(CastControl::Error(format!(
                "could not create a PipeWire stream: {err}"
            )));
            finish(&control);
            return;
        }
    };

    // The inward channel: the only sanctioned way to poke a *running* pw
    // main loop from another thread. `quit()` from outside would be a race
    // on a non-`Sync` object; this instead wakes the loop through a pipe fd
    // it is already polling, so the callback runs *on* the loop's thread.
    let _stop_receiver = stop_rx.attach(main_loop.loop_(), {
        let main_loop = main_loop.clone();
        move |_| main_loop.quit()
    });

    let state = StreamState {
        control: control.clone(),
        frames,
        dropped,
        format: None,
        mappings: HashMap::new(),
        sequence: 0,
        failed: false,
    };

    let listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed({
            let main_loop = main_loop.clone();
            move |_stream, state, old, new| {
                eprintln!("saola-capture: screencast: pipewire stream {old:?} -> {new:?}");
                if let pipewire::stream::StreamState::Error(message) = &new {
                    state.fail(format!("the PipeWire stream failed: {message}"));
                    main_loop.quit();
                }
            }
        })
        .param_changed(|stream, state, id, param| {
            on_param_changed(stream, state, id, param);
        })
        .remove_buffer(|_stream, state, buffer| {
            // SAFETY: pipewire hands this callback a live `pw_buffer` it
            // still owns for the duration of the call; we only read the
            // `datas` array to learn which fd's mapping to release, and
            // never retain the pointer.
            if let Some(fd) = unsafe { first_data_fd(buffer) } {
                state.mappings.remove(&fd);
            }
        })
        .process(|stream, state| {
            on_process(stream, state);
        })
        .register();

    let listener = match listener {
        Ok(listener) => listener,
        Err(err) => {
            let _ = control.send(CastControl::Error(format!(
                "could not register PipeWire stream callbacks: {err}"
            )));
            finish(&control);
            return;
        }
    };

    let offer = match enum_format_pod() {
        Ok(offer) => offer,
        Err(err) => {
            let _ = control.send(CastControl::Error(err));
            finish(&control);
            return;
        }
    };
    let offer_pod = match pipewire::spa::pod::Pod::from_bytes(&offer) {
        Some(pod) => pod,
        None => {
            let _ = control.send(CastControl::Error(
                "the SPA format offer did not serialize into a valid pod".to_string(),
            ));
            finish(&control);
            return;
        }
    };
    let mut params = [offer_pod];

    // `MAP_BUFFERS` is set even though CAPTURE-RESEARCH §2.3 gotcha 1 proved
    // it does *not* map dmabufs: it costs nothing, it is what the probe used
    // (so this is the configuration the evidence covers), and it would do
    // the right thing for any non-dmabuf path a future compositor offers.
    // The dmabuf mapping is done by hand regardless.
    if let Err(err) = stream.connect(
        pipewire::spa::utils::Direction::Input,
        Some(node_id),
        pipewire::stream::StreamFlags::AUTOCONNECT
            | pipewire::stream::StreamFlags::MAP_BUFFERS
            | pipewire::stream::StreamFlags::RT_PROCESS,
        &mut params,
    ) {
        let _ = control.send(CastControl::Error(format!(
            "could not connect to PipeWire node {node_id}: {err}"
        )));
        finish(&control);
        return;
    }

    // Blocks here until `LoopCommand::Stop` arrives or a stream error quits
    // the loop from `state_changed`.
    main_loop.run();

    if let Err(err) = stream.disconnect() {
        eprintln!("saola-capture: screencast: pw_stream_disconnect failed: {err}");
    }
    // Explicit rather than implicit: the listener holds the user data (and
    // therefore every dmabuf mapping), so dropping it here unmaps everything
    // before the stream and core go away.
    drop(listener);
    finish(&control);
}

/// `param_changed`: the negotiated format arrives, and the buffer
/// requirements go back.
fn on_param_changed(
    stream: &pipewire::stream::Stream,
    state: &mut StreamState,
    id: u32,
    param: Option<&pipewire::spa::pod::Pod>,
) {
    use pipewire::spa::param::format::{MediaSubtype, MediaType};

    // A `None` param means "the format was cleared" — the shape a failed
    // negotiation takes (CAPTURE-RESEARCH §2.1's transcript shows exactly
    // this, `id=4 -> NULL (cleared)`, right before `no more input formats`).
    // It is not itself the error; the `StreamState::Error` that follows is,
    // and `state_changed` reports that. Dropping the cached mappings here is
    // the important part: after a format change the old buffers are gone.
    let Some(param) = param else {
        state.mappings.clear();
        return;
    };
    if id != pipewire::spa::param::ParamType::Format.as_raw() {
        return;
    }

    let (media_type, media_subtype) = match pipewire::spa::param::format_utils::parse_format(param)
    {
        Ok(pair) => pair,
        Err(err) => {
            state.fail(format!("could not parse the negotiated SPA format: {err}"));
            return;
        }
    };
    if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
        state.fail(format!(
            "the cast negotiated {media_type:?}/{media_subtype:?}, but saola-capture only \
             consumes raw video"
        ));
        return;
    }

    let mut info = pipewire::spa::param::video::VideoInfoRaw::new();
    if let Err(err) = info.parse(param) {
        state.fail(format!(
            "could not parse the negotiated video format: {err}"
        ));
        return;
    }

    let size = info.size();
    if size.width == 0 || size.height == 0 {
        state.fail(format!(
            "the cast negotiated a {}x{} frame, which cannot be recorded",
            size.width, size.height
        ));
        return;
    }

    let format = NegotiatedFormat {
        width: size.width,
        height: size.height,
        modifier: info.modifier(),
        framerate: (info.framerate().num, info.framerate().denom),
        max_framerate: (info.max_framerate().num, info.max_framerate().denom),
    };

    // A format change mid-stream invalidates every buffer we mapped.
    state.mappings.clear();
    state.format = Some(format);
    let _ = state.control.send(CastControl::Negotiated(format));

    match buffers_pod(&format) {
        Ok(bytes) => match pipewire::spa::pod::Pod::from_bytes(&bytes) {
            Some(pod) => {
                let mut reply = [pod];
                if let Err(err) = stream.update_params(&mut reply) {
                    state.fail(format!("could not reply with SPA_PARAM_Buffers: {err}"));
                }
            }
            None => state.fail("the SPA_PARAM_Buffers reply is not a valid pod".to_string()),
        },
        Err(err) => state.fail(err),
    }
}

/// `process`: one buffer is ready.
fn on_process(stream: &pipewire::stream::Stream, state: &mut StreamState) {
    use pipewire::spa::buffer::DataType;

    if state.failed {
        return;
    }
    let Some(format) = state.format else {
        // Frames before a format is impossible in practice; ignoring them
        // beats guessing a layout.
        return;
    };

    // `dequeue_buffer` returns a guard that re-queues the buffer on drop, so
    // every `return` below hands it straight back to the producer — there is
    // no path that starves the pool.
    let Some(mut buffer) = stream.dequeue_buffer() else {
        // Normal under load, not an error: the producer has nothing spare.
        return;
    };
    let datas = buffer.datas_mut();
    let Some(data) = datas.first_mut() else {
        return;
    };

    if data.type_() != DataType::DmaBuf {
        state.fail(format!(
            "the cast produced a {:?} buffer; saola-capture implements only the dmabuf/LINEAR \
             path, because niri refuses shm at both the format and the buffer-allocation layer \
             (CAPTURE-RESEARCH §2.1/D4)",
            data.type_()
        ));
        return;
    }

    let fd = data.fd();
    if fd < 0 {
        state.fail("the cast produced a dmabuf with no file descriptor".to_string());
        return;
    }

    // `Data::chunk()` asserts the chunk pointer is non-null, which would be
    // a panic on a runtime path — checked here instead (CLAUDE.md's
    // no-panic rule does not make exceptions for "cannot happen").
    if data.as_raw().chunk.is_null() {
        state.fail("the cast produced a buffer with no chunk descriptor".to_string());
        return;
    }
    let map_offset = data.as_raw().mapoffset as usize;
    let stride = data.chunk().stride();
    if stride <= 0 {
        state.fail(format!(
            "the cast reported a stride of {stride}, which cannot be read"
        ));
        return;
    }
    let stride = stride as usize;

    // Map on first sight of this fd, then reuse. Note what is carried out
    // of the map: the pointer and length are *copied out* and the borrow of
    // `state.mappings` ends immediately, because every failure path below
    // calls `state.fail` — a second mutable borrow of `state`, which the
    // borrow checker rightly refuses while a `&FrameMapping` is alive. The
    // raw pointer stays valid regardless: its allocation is owned by
    // `state.mappings`, and nothing between here and the copy removes it.
    let existing = state.mappings.get(&fd).map(|m| (m.as_ptr(), m.len));
    let (base, map_len) = match existing {
        Some(pair) => pair,
        None => match FrameMapping::new(fd) {
            Ok(mapping) => {
                let pair = (mapping.as_ptr(), mapping.len);
                state.mappings.insert(fd, mapping);
                pair
            }
            Err(err) => {
                state.fail(format!("could not map the cast's dmabuf: {err}"));
                return;
            }
        },
    };

    let required = required_mapping_len(map_offset, stride, format.width, format.height);
    let Some(required) = required else {
        state.fail("the cast's frame geometry overflows a usize".to_string());
        return;
    };
    if map_len < required {
        state.fail(format!(
            "the cast's dmabuf is {map_len} bytes but a {}x{} frame at stride {stride} (offset \
             {map_offset}) needs {required}",
            format.width, format.height
        ));
        return;
    }

    // Bracket the CPU read, as the kernel's dma-buf contract requires: the
    // producer may have written through the GPU, and without this the CPU
    // can see stale cache lines.
    dma_buf_sync(fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);
    // SAFETY: `base` points at a mapping of `map_len` readable bytes that
    // `state.mappings` still owns (nothing between the lookup above and
    // here removes it), and `required <= map_len` was just checked, so
    // `copy_packed_rows` reads only within the mapping.
    let bytes = unsafe { copy_packed_rows(base, map_offset, stride, format.width, format.height) };
    dma_buf_sync(fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);

    let frame = VideoFrame {
        width: format.width,
        height: format.height,
        bytes,
        sequence: state.sequence,
        captured_at: Instant::now(),
    };

    // **The backpressure rule, in three lines** (PLAN.md: "The PipeWire
    // thread never blocks on the encoder: bounded channel, drop frames and
    // log when full"). `try_send`, never `send`: blocking here would stall
    // the compositor's own producer, which is far worse than a dropped
    // frame in a variable-rate recording.
    match state.frames.try_send(frame) {
        Ok(()) => state.sequence += 1,
        Err(TrySendError::Full(_)) => {
            let dropped = state.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // Log the first few and then every 60th, so a persistently slow
            // encoder leaves evidence without flooding the journal.
            if dropped <= 3 || dropped.is_multiple_of(60) {
                eprintln!(
                    "saola-capture: screencast: frame queue full, dropped frame ({dropped} so far)"
                );
            }
        }
        Err(TrySendError::Disconnected(_)) => {
            // Nobody is listening any more. Not an error worth reporting —
            // the owner dropped the stream, which is how a normal stop
            // begins.
        }
    }
}

/// Reads `buffer->buffer->datas[0].fd` out of a raw `pw_buffer`.
///
/// # Safety
///
/// `buffer` must be a live `pw_buffer` pointer owned by PipeWire for the
/// duration of the call (which is exactly what the `add_buffer`/
/// `remove_buffer` callbacks are handed).
unsafe fn first_data_fd(buffer: *mut pipewire::sys::pw_buffer) -> Option<RawFd> {
    if buffer.is_null() {
        return None;
    }
    let spa_buffer = (*buffer).buffer;
    if spa_buffer.is_null() || (*spa_buffer).n_datas == 0 || (*spa_buffer).datas.is_null() {
        return None;
    }
    Some((*(*spa_buffer).datas).fd as RawFd)
}

/// A read-only `mmap` of one dmabuf, unmapped on drop.
struct FrameMapping {
    ptr: *mut libc::c_void,
    len: usize,
}

impl FrameMapping {
    /// Map the whole fd from offset 0.
    ///
    /// The length comes from `lseek(fd, 0, SEEK_END)` and **not** from
    /// `maxsize`/`chunk->size`, which are dummies on this path (both were
    /// `1` in every probe frame — CAPTURE-RESEARCH §2.3 gotcha 2). Mapping
    /// from 0 rather than from `mapoffset` also means one mapping stays
    /// correct if two buffers ever share an allocation at different
    /// offsets; the offset is applied when reading rows instead.
    fn new(fd: RawFd) -> Result<Self, std::io::Error> {
        // SAFETY: `fd` is a live dmabuf fd owned by PipeWire for at least
        // the lifetime of this mapping (it is released in `remove_buffer`);
        // `lseek` only reads the file's size.
        let len = unsafe { libc::lseek(fd, 0, libc::SEEK_END) };
        if len <= 0 {
            return Err(std::io::Error::other(format!(
                "the dmabuf reported a size of {len}"
            )));
        }
        let len = len as usize;

        // SAFETY: a null hint with a non-zero length and a valid fd is the
        // ordinary `mmap` contract; `MAP_FAILED` is checked immediately, and
        // `PROT_READ`/`MAP_SHARED` is exactly what the Stage 2 probe proved
        // works for a LINEAR dmabuf with no GBM/EGL import.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(FrameMapping { ptr, len })
    }

    fn as_ptr(&self) -> *const u8 {
        self.ptr.cast::<u8>()
    }
}

/// `DMA_BUF_IOCTL_SYNC` on a dmabuf fd.
///
/// A failure is logged, not fatal: on every tested path it succeeds, and if
/// a kernel ever refuses it the pixels are still *readable* — just possibly
/// stale — which is a degraded recording rather than no recording.
///
/// A free function taking the fd rather than a method on [`FrameMapping`]
/// so the caller can finish borrowing the map before bracketing the read
/// (see [`on_process`]).
fn dma_buf_sync(fd: RawFd, flags: u64) {
    let mut sync = DmaBufSync { flags };
    // SAFETY: `fd` is a live dmabuf fd owned by PipeWire for the duration of
    // the call, and `DmaBufSync` is the exact `struct dma_buf_sync` the
    // ioctl reads.
    let result = unsafe { libc::ioctl(fd, DMA_BUF_IOCTL_SYNC, &mut sync) };
    if result != 0 {
        eprintln!(
            "saola-capture: screencast: DMA_BUF_IOCTL_SYNC failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

impl Drop for FrameMapping {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` are exactly what `mmap` returned and this type
        // is the only owner of that mapping.
        unsafe {
            libc::munmap(self.ptr, self.len);
        }
    }
}

/// How many bytes of a mapping a `width × height` frame at `stride`,
/// starting at `map_offset`, actually touches.
///
/// Note it is **not** `map_offset + stride * height`: the last row needs
/// only `width * 4` bytes, and demanding a full stride past it would reject
/// a perfectly good buffer whose allocation ends tight against the last row.
/// `None` on arithmetic overflow.
fn required_mapping_len(
    map_offset: usize,
    stride: usize,
    width: u32,
    height: u32,
) -> Option<usize> {
    let row_bytes = (width as usize).checked_mul(4)?;
    let last_row_start = stride.checked_mul((height as usize).checked_sub(1)?)?;
    map_offset
        .checked_add(last_row_start)
        .and_then(|start| start.checked_add(row_bytes))
}

/// Copy `height` rows of `width * 4` bytes from a strided source into a
/// packed `Vec`.
///
/// **This is the one place CAPTURE-RESEARCH §2.3 gotcha 3 lives.** The
/// source advances by `stride` per row (the compositor's *output* pitch, not
/// `width * 4`); the destination advances by `width * 4`. Getting these two
/// confused shears the image, which is why the row arithmetic is factored
/// out here and unit-tested rather than being inlined in the callback.
///
/// # Safety
///
/// `base` must point to at least
/// `required_mapping_len(map_offset, stride, width, height)` readable bytes.
unsafe fn copy_packed_rows(
    base: *const u8,
    map_offset: usize,
    stride: usize,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let row_bytes = width as usize * 4;
    let mut packed = vec![0u8; row_bytes * height as usize];
    for row in 0..height as usize {
        let src = base.add(map_offset + row * stride);
        let dst_start = row * row_bytes;
        std::ptr::copy_nonoverlapping(src, packed.as_mut_ptr().add(dst_start), row_bytes);
    }
    packed
}

// ---------------------------------------------------------------------------
// SPA pods
// ---------------------------------------------------------------------------
//
// Teaching note on SPA pods: a "pod" is SPA's self-describing binary value
// format — a little tagged tree of ints, ids, rectangles, fractions,
// *objects* (key/value property bags) and *choices* (a value plus a set of
// acceptable alternatives). Format negotiation is two sides exchanging pods
// until they intersect. `pipewire-rs` gives two ways to build one: a raw
// `spa_pod_builder` wrapper (every push is `unsafe`, because the frames it
// uses must not move) and a safe `Value`/`Object`/`Property` tree fed to
// `PodSerializer`. The tree is used here — the pods are small, built once
// per stream, and being able to read the structure at a glance matters more
// than the allocation.
//
// Two flag namespaces are easy to conflate and are *not* the same thing:
//   * `PropertyFlags` (MANDATORY, DONT_FIXATE) sit on the **property**;
//   * `ChoiceFlags` sit on the **choice** and are empty here — the Stage 2
//     probe passed 0 for `spa_pod_builder_push_choice`'s flags argument.

/// The `SPA_PARAM_EnumFormat` this consumer offers, byte-for-byte equivalent
/// to the Stage 2 probe's `dmabuf` mode (`pwprobe.c`), which is the only
/// offer niri was ever observed to accept.
///
/// The `modifier` property is the whole trick: `MANDATORY` says "I cannot
/// use a format without one", `DONT_FIXATE` says "these are candidates, you
/// pick", and the choice lists `LINEAR` twice — once as the default, once as
/// the single alternative — because a SPA `Enum` choice is *default followed
/// by alternatives*, and the probe wrote it that way.
fn enum_format_pod() -> Result<Vec<u8>, String> {
    use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use pipewire::spa::param::video::VideoFormat;
    use pipewire::spa::param::ParamType;
    use pipewire::spa::pod::{ChoiceValue, Object, Property, PropertyFlags, Value};
    use pipewire::spa::utils::{
        Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes,
    };

    let object = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(
                FormatProperties::MediaType.as_raw(),
                Value::Id(Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                FormatProperties::MediaSubtype.as_raw(),
                Value::Id(Id(MediaSubtype::Raw.as_raw())),
            ),
            Property::new(
                FormatProperties::VideoFormat.as_raw(),
                Value::Id(Id(VideoFormat::BGRx.as_raw())),
            ),
            Property {
                key: FormatProperties::VideoModifier.as_raw(),
                flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
                value: Value::Choice(ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: DRM_FORMAT_MOD_LINEAR as i64,
                        alternatives: vec![DRM_FORMAT_MOD_LINEAR as i64],
                    },
                ))),
            },
            Property::new(
                FormatProperties::VideoSize.as_raw(),
                Value::Choice(ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Rectangle {
                            width: 1920,
                            height: 1080,
                        },
                        min: Rectangle {
                            width: 1,
                            height: 1,
                        },
                        max: Rectangle {
                            width: 8192,
                            height: 8192,
                        },
                    },
                ))),
            ),
            Property::new(
                FormatProperties::VideoFramerate.as_raw(),
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Fraction { num: 60, denom: 1 },
                        min: Fraction { num: 0, denom: 1 },
                        max: Fraction {
                            num: 1000,
                            denom: 1,
                        },
                    },
                ))),
            ),
        ],
    };

    serialize_pod(Value::Object(object))
}

/// The `SPA_PARAM_Buffers` reply, sent from `param_changed` once a format
/// exists.
///
/// `dataType` is a `Flags` choice containing **only** `1 << SPA_DATA_DmaBuf`.
/// CAPTURE-RESEARCH §2.1 layer 2 is exactly the experiment that proves why:
/// with `MemFd|MemPtr` here (and a perfectly negotiated format above) niri
/// answers `error alloc buffers: Invalid argument`.
///
/// `size`/`stride` are the *hints* a consumer offers; the producer is free
/// to allocate at its own pitch and does (§2.3 gotcha 3), which is why
/// nothing downstream may rely on these numbers.
fn buffers_pod(format: &NegotiatedFormat) -> Result<Vec<u8>, String> {
    use pipewire::spa::param::ParamType;
    use pipewire::spa::pod::{ChoiceValue, Object, Property, Value};
    use pipewire::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, SpaTypes};

    let stride = i32::try_from(format.width.saturating_mul(4))
        .map_err(|_| format!("a width of {} is too large to record", format.width))?;
    let size = i32::try_from(format.packed_len()).map_err(|_| {
        format!(
            "a {}x{} frame is too large to record",
            format.width, format.height
        )
    })?;
    let dma_buf_only = 1i32 << pipewire::spa::sys::SPA_DATA_DmaBuf;

    let object = Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(
                pipewire::spa::sys::SPA_PARAM_BUFFERS_buffers,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: 4,
                        min: 2,
                        max: 16,
                    },
                ))),
            ),
            Property::new(pipewire::spa::sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            Property::new(pipewire::spa::sys::SPA_PARAM_BUFFERS_size, Value::Int(size)),
            Property::new(
                pipewire::spa::sys::SPA_PARAM_BUFFERS_stride,
                Value::Int(stride),
            ),
            Property::new(
                pipewire::spa::sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: dma_buf_only,
                        flags: vec![dma_buf_only],
                    },
                ))),
            ),
        ],
    };

    serialize_pod(Value::Object(object))
}

/// `Value` → bytes. Factored out so the two pod builders above share the
/// one error path, and so neither has to spell out `PodSerializer`'s
/// `Cursor`-in/`Cursor`-out shape.
fn serialize_pod(value: pipewire::spa::pod::Value) -> Result<Vec<u8>, String> {
    pipewire::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &value,
    )
    .map(|(cursor, _len)| cursor.into_inner())
    .map_err(|err| format!("could not serialize a SPA pod: {err}"))
}

// ---------------------------------------------------------------------------
// `record start --dry-run`
// ---------------------------------------------------------------------------

/// What one dry run observed. Every field is something a human can sanity
/// check against the machine in front of them.
#[derive(Debug, Clone)]
pub struct DryRunReport {
    pub target: CastTarget,
    pub node_id: u32,
    pub format: Option<NegotiatedFormat>,
    /// `Stream.Parameters`, in logical pixels — the *other* coordinate
    /// space, printed alongside the physical one on purpose.
    pub parameters: Option<StreamParameters>,
    pub frames: u64,
    pub dropped: u64,
    /// How long after attaching the first frame arrived.
    pub first_frame_after: Option<Duration>,
    /// Shortest, longest and mean gap between consecutive frames.
    pub gaps: Option<(Duration, Duration, Duration)>,
    pub observed_for: Duration,
    /// niri's own view of the cast at the end of the window — the
    /// independent confirmation that `is_active` flipped once our consumer
    /// attached (CAPTURE-RESEARCH D11).
    pub niri_cast: Option<CastSummary>,
    /// A fatal error the pw thread reported, if any.
    pub error: Option<String>,
}

impl DryRunReport {
    /// Frames per second over the observation window. `None` if fewer than
    /// two frames arrived (one frame gives no rate at all, and a
    /// damage-driven cast really can deliver one frame in six seconds —
    /// CAPTURE-RESEARCH §2.3 gotcha 4).
    pub fn average_fps(&self) -> Option<f64> {
        if self.frames < 2 || self.observed_for.is_zero() {
            return None;
        }
        Some(self.frames as f64 / self.observed_for.as_secs_f64())
    }
}

impl fmt::Display for DryRunReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "dry run: {} (PipeWire node {})",
            self.target, self.node_id
        )?;
        match &self.format {
            Some(format) => {
                let (even_w, even_h) = format.even_dimensions();
                writeln!(f, "  negotiated: {format}")?;
                writeln!(
                    f,
                    "  encoder would use: -f rawvideo -pixel_format bgr0 -video_size {even_w}x{even_h}"
                )?;
            }
            None => writeln!(f, "  negotiated: (no format was ever agreed)")?,
        }
        if let Some(parameters) = self.parameters {
            writeln!(
                f,
                "  logical geometry: {}x{} at {},{}",
                parameters.size.0, parameters.size.1, parameters.position.0, parameters.position.1
            )?;
        }
        writeln!(
            f,
            "  frames: {} in {:.1}s (dropped {})",
            self.frames,
            self.observed_for.as_secs_f64(),
            self.dropped
        )?;
        if let Some(fps) = self.average_fps() {
            writeln!(f, "  average cadence: {fps:.1} fps")?;
        }
        if let Some(first) = self.first_frame_after {
            writeln!(
                f,
                "  first frame after: {:.0} ms",
                first.as_secs_f64() * 1000.0
            )?;
        }
        if let Some((min, max, mean)) = self.gaps {
            writeln!(
                f,
                "  frame gaps: min {:.1} ms, mean {:.1} ms, max {:.1} ms",
                min.as_secs_f64() * 1000.0,
                mean.as_secs_f64() * 1000.0,
                max.as_secs_f64() * 1000.0
            )?;
        }
        match &self.niri_cast {
            Some(cast) => writeln!(f, "  niri sees: {cast}")?,
            None => writeln!(
                f,
                "  niri sees: (no matching cast — is niri's IPC reachable?)"
            )?,
        }
        if let Some(error) = &self.error {
            writeln!(f, "  error: {error}")?;
        }
        write!(f, "  nothing was written to disk")
    }
}

/// `record start --dry-run`: negotiate, watch frames for `duration`, write
/// nothing, tear everything down (PLAN.md Stage 10 task 4).
///
/// Runs **in the calling process**, not through the daemon, and that is a
/// deliberate choice rather than an omission:
///
/// - it changes no recording state, so there is nothing for the daemon (the
///   owner of recording state) to own;
/// - its entire product is a *log*, which belongs on the terminal that asked
///   for it — routing it through D-Bus would mean either inventing a
///   log-streaming method (the `io.saola.Capture1` surface is frozen) or
///   telling the user to go read the daemon's journal;
/// - it therefore cannot disturb a running daemon or a real recording, which
///   is exactly what you want from the command whose job is "prove the
///   plumbing works on this machine".
///
/// `shot --no-daemon` is the established precedent for "a CLI verb doing
/// capture work in-process".
pub async fn dry_run(
    target: CastTarget,
    cursor: CursorMode,
    duration: Duration,
) -> Result<DryRunReport, CastError> {
    let connection = Connection::session()
        .await
        .map_err(|err| CastError::Bus(err.to_string()))?;

    let session = CastSession::open(&connection, &target, cursor).await?;
    let node_id = session.node_id();
    let parameters = session.parameters();

    let stream = PipeWireStream::connect(node_id)?;

    // The receivers are blocking, so the observation window runs on the
    // blocking pool rather than parking the executor — the same
    // `Handle::try_current()` guard `dbus::run_blocking` uses, because
    // `spawn_blocking` *panics* outside a runtime and the no-panic rule has
    // no "cannot happen" exemption.
    let (stream, mut report) = match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::spawn_blocking(move || {
            let report = observe(&stream, duration);
            (stream, report)
        })
        .await
        .map_err(|err| CastError::Thread(err.to_string()))?,
        Err(_) => {
            let report = observe(&stream, duration);
            (stream, report)
        }
    };

    // niri's independent view, read while the consumer is still attached —
    // reading it after teardown would only ever say "gone".
    report.niri_cast = casts_snapshot()
        .as_deref()
        .and_then(|casts| cast_for_node(casts, node_id))
        .map(summarize_cast);
    report.target = session.target().clone();
    report.node_id = node_id;
    report.parameters = parameters;

    // Teardown, in the binding order: consumer first, then the session.
    stream.stop();
    session.close().await;

    // CAPTURE-RESEARCH §2.4 is explicit that "negotiation produced no
    // format" is a **first-class, user-visible error path**, since that is
    // the shape every unsupported case takes (a compositor that refuses
    // LINEAR, a node that offers only tiled modifiers). So a run that never
    // agreed a format is an `Err`, not a report with a sad field — while a
    // run that negotiated fine and *then* hit trouble keeps its report,
    // because everything it did learn is still worth printing.
    if report.format.is_none() {
        return Err(CastError::PipeWire(report.error.unwrap_or_else(|| {
            "the stream never negotiated a format".to_string()
        })));
    }

    Ok(report)
}

/// The blocking half of [`dry_run`]: drain both channels until `duration`
/// elapses. Synchronous by design — see the module doc on why the pw side
/// speaks `std::sync::mpsc`.
fn observe(stream: &PipeWireStream, duration: Duration) -> DryRunReport {
    let started = Instant::now();
    let deadline = started + duration;

    let mut format = None;
    let mut error = None;
    let mut frames = 0u64;
    let mut first_frame_after = None;
    let mut previous: Option<Instant> = None;
    let mut gap_min: Option<Duration> = None;
    let mut gap_max: Option<Duration> = None;
    let mut gap_total = Duration::ZERO;
    let mut gap_count = 0u32;

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;

        // Control messages are rare, so they are drained non-blockingly and
        // the *frame* channel is the one this loop parks on.
        while let Ok(message) = stream.control().try_recv() {
            match message {
                CastControl::Negotiated(negotiated) => format = Some(negotiated),
                CastControl::Error(message) => {
                    error.get_or_insert(message);
                }
                CastControl::Ended => {
                    error.get_or_insert_with(|| {
                        "the PipeWire stream ended before the dry run finished".to_string()
                    });
                }
            }
        }
        if error.is_some() {
            break;
        }

        match stream.frames().recv_timeout(remaining) {
            Ok(frame) => {
                frames += 1;
                if first_frame_after.is_none() {
                    first_frame_after = Some(frame.captured_at.saturating_duration_since(started));
                }
                if let Some(previous) = previous {
                    let gap = frame.captured_at.saturating_duration_since(previous);
                    gap_min = Some(gap_min.map_or(gap, |min: Duration| min.min(gap)));
                    gap_max = Some(gap_max.map_or(gap, |max: Duration| max.max(gap)));
                    gap_total += gap;
                    gap_count += 1;
                }
                previous = Some(frame.captured_at);
                // The frame's pixels are deliberately dropped here — "writes
                // nothing" is the whole contract of a dry run.
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    // One last non-blocking sweep, so a format or error that landed in the
    // final instant still shows up in the report.
    while let Ok(message) = stream.control().try_recv() {
        match message {
            CastControl::Negotiated(negotiated) => format = Some(negotiated),
            CastControl::Error(message) => {
                error.get_or_insert(message);
            }
            // Ignored here, unlike in the loop above: this sweep runs after
            // the deadline, so an `Ended` arriving now is indistinguishable
            // from the teardown the caller is about to do anyway. Calling
            // that "the stream ended early" would be noise.
            CastControl::Ended => {}
        }
    }

    let gaps = match (gap_min, gap_max) {
        (Some(min), Some(max)) if gap_count > 0 => Some((min, max, gap_total / gap_count)),
        _ => None,
    };

    DryRunReport {
        // Overwritten by the caller, which knows these without a channel.
        target: CastTarget::Monitor {
            connector: String::new(),
        },
        node_id: 0,
        format,
        parameters: None,
        frames,
        dropped: stream.dropped_frames(),
        first_frame_after,
        gaps,
        observed_for: started.elapsed(),
        niri_cast: None,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- pure geometry: the stride/padding rules Stage 11 inherits --------

    #[test]
    fn required_length_uses_the_last_row_not_a_full_stride() {
        // 4 rows of 2 px (8 packed bytes) at a 16-byte stride: the last row
        // needs only its own 8 bytes, so the answer is 3*16 + 8, not 4*16.
        assert_eq!(required_mapping_len(0, 16, 2, 4), Some(56));
    }

    #[test]
    fn required_length_honours_the_map_offset() {
        assert_eq!(required_mapping_len(100, 16, 2, 4), Some(156));
    }

    #[test]
    fn required_length_rejects_a_zero_height_frame() {
        assert_eq!(required_mapping_len(0, 16, 2, 0), None);
    }

    #[test]
    fn required_length_saturates_rather_than_overflowing() {
        assert_eq!(required_mapping_len(usize::MAX, 16, 2, 4), None);
    }

    #[test]
    fn copy_packed_rows_drops_the_stride_padding() {
        // The exact shape of CAPTURE-RESEARCH §2.3 gotcha 3 in miniature: a
        // 2x3 image living in a buffer whose rows are 4 px wide.
        let width = 2u32;
        let height = 3u32;
        let stride = 16usize; // 4 px * 4 bytes
        let mut source = vec![0u8; stride * height as usize];
        for row in 0..height as usize {
            for byte in 0..stride {
                // Real pixels get a recognizable value; padding gets 0xEE.
                source[row * stride + byte] = if byte < 8 {
                    (row * 8 + byte) as u8
                } else {
                    0xEE
                };
            }
        }

        // SAFETY: `source` is longer than the required length for this
        // geometry, computed just above.
        let packed = unsafe { copy_packed_rows(source.as_ptr(), 0, stride, width, height) };

        assert_eq!(packed.len(), 24);
        assert_eq!(
            packed,
            vec![
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23
            ]
        );
        assert!(
            !packed.contains(&0xEE),
            "stride padding leaked into the packed frame"
        );
    }

    #[test]
    fn copy_packed_rows_applies_the_map_offset() {
        let stride = 8usize;
        let mut source = vec![0xEEu8; 4 + stride * 2];
        source[4..8].copy_from_slice(&[1, 2, 3, 4]);
        source[12..16].copy_from_slice(&[5, 6, 7, 8]);

        // SAFETY: the buffer is exactly `required_mapping_len(4, 8, 1, 2)`
        // = 4 + 8 + 4 = 16 bytes long, which is `source.len()`.
        let packed = unsafe { copy_packed_rows(source.as_ptr(), 4, stride, 1, 2) };
        assert_eq!(packed, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    // -- the negotiated format's derived numbers -------------------------

    fn format(width: u32, height: u32) -> NegotiatedFormat {
        NegotiatedFormat {
            width,
            height,
            modifier: DRM_FORMAT_MOD_LINEAR,
            framerate: (0, 1),
            max_framerate: (60000, 1000),
        }
    }

    #[test]
    fn even_dimensions_round_down_like_the_vaapi_encoder_needs() {
        // The exact case CAPTURE-RESEARCH §3.6 measured.
        assert_eq!(format(2507, 1457).even_dimensions(), (2506, 1456));
        assert_eq!(format(2560, 1600).even_dimensions(), (2560, 1600));
    }

    #[test]
    fn packed_len_is_four_bytes_per_pixel() {
        assert_eq!(format(2560, 1600).packed_len(), 16_384_000);
    }

    #[test]
    fn format_display_names_every_negotiated_field() {
        let text = format(2560, 1600).to_string();
        assert!(text.contains("BGRx 2560x1600"), "{text}");
        assert!(text.contains("modifier 0x0"), "{text}");
        assert!(text.contains("maxFramerate 60000/1000"), "{text}");
    }

    // -- SPA pods: the bytes actually serialize --------------------------

    #[test]
    fn the_enum_format_offer_serializes_to_a_valid_pod() {
        let bytes = enum_format_pod().expect("the offer must serialize");
        assert!(
            pipewire::spa::pod::Pod::from_bytes(&bytes).is_some(),
            "the EnumFormat offer did not round-trip through Pod::from_bytes"
        );
        // A pod is 8-byte aligned and padded; a malformed one would not be.
        assert_eq!(bytes.len() % 8, 0);
    }

    #[test]
    fn the_enum_format_offer_carries_the_dont_fixate_modifier() {
        // Rebuilt here rather than reparsed: the point is that the *flags*
        // this stage depends on exist under the chosen cargo feature gate
        // (`DONT_FIXATE` is `#[cfg(feature = "v0_3_33")]` in libspa), so a
        // future feature-gate change breaks a test instead of a negotiation.
        use pipewire::spa::pod::PropertyFlags;
        let flags = PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE;
        assert!(flags.contains(PropertyFlags::MANDATORY));
        assert!(flags.contains(PropertyFlags::DONT_FIXATE));
    }

    #[test]
    fn the_buffers_reply_serializes_and_asks_for_dmabuf_only() {
        let bytes = buffers_pod(&format(2560, 1600)).expect("the buffers reply must serialize");
        assert!(pipewire::spa::pod::Pod::from_bytes(&bytes).is_some());

        // The dataType mask this stage insists on: 1 << SPA_DATA_DmaBuf,
        // and nothing else. Asserted as a number so a mistaken MemFd/MemPtr
        // addition (which CAPTURE-RESEARCH §2.1 proves niri rejects at
        // allocation time) fails here rather than at runtime.
        let dma_buf_only = 1u32 << pipewire::spa::sys::SPA_DATA_DmaBuf;
        assert_eq!(dma_buf_only, 1 << 3);
        assert_eq!(dma_buf_only & (1 << pipewire::spa::sys::SPA_DATA_MemFd), 0);
        assert_eq!(dma_buf_only & (1 << pipewire::spa::sys::SPA_DATA_MemPtr), 0);
    }

    #[test]
    fn the_buffers_reply_refuses_an_unencodably_large_frame() {
        assert!(buffers_pod(&format(u32::MAX, u32::MAX)).is_err());
    }

    // -- cursor mode -----------------------------------------------------

    #[test]
    fn cursor_mode_uses_the_mutter_enum_not_the_portal_bitmask() {
        // Mutter's enum is 0/1/2; the portal's bitmask would make "embedded"
        // 2 and "metadata" 4, and a live probe rejected 3 and 4 outright.
        assert_eq!(CursorMode::Hidden.as_u32(), 0);
        assert_eq!(CursorMode::Embedded.as_u32(), 1);
        assert_eq!(CursorMode::from_cursor_option(true), CursorMode::Embedded);
        assert_eq!(CursorMode::from_cursor_option(false), CursorMode::Hidden);
    }

    // -- niri's cast list ------------------------------------------------

    fn cast(stream_id: u64, node: Option<u32>) -> niri_ipc::Cast {
        niri_ipc::Cast {
            stream_id,
            session_id: 7,
            kind: niri_ipc::CastKind::PipeWire,
            target: niri_ipc::CastTarget::Output {
                name: "eDP-1".to_string(),
            },
            is_dynamic_target: false,
            is_active: true,
            pid: None,
            pw_node_id: node,
        }
    }

    #[test]
    fn cast_for_node_matches_on_the_pipewire_node_id() {
        let casts = vec![cast(1, Some(70)), cast(2, Some(71))];
        assert_eq!(cast_for_node(&casts, 71).map(|c| c.stream_id), Some(2));
        assert!(cast_for_node(&casts, 99).is_none());
    }

    #[test]
    fn cast_for_node_ignores_a_cast_that_has_no_node_yet() {
        // niri's own docs: `pw_node_id` is None "for PipeWire casts before
        // the node is created (when the cast is just starting up)".
        let casts = vec![cast(1, None)];
        assert!(cast_for_node(&casts, 0).is_none());
    }

    #[test]
    fn summarize_cast_flattens_the_target() {
        let summary = summarize_cast(&cast(3, Some(70)));
        assert_eq!(summary.stream_id, 3);
        assert_eq!(summary.target, "output eDP-1");
        assert!(summary.is_active);

        let mut window = cast(4, Some(71));
        window.target = niri_ipc::CastTarget::Window { id: 19 };
        assert_eq!(summarize_cast(&window).target, "window 19");
    }

    // -- the dry-run report ----------------------------------------------

    fn report(frames: u64, observed: Duration) -> DryRunReport {
        DryRunReport {
            target: CastTarget::Monitor {
                connector: "eDP-1".to_string(),
            },
            node_id: 70,
            format: Some(format(2560, 1600)),
            parameters: Some(StreamParameters {
                position: (0, 0),
                size: (1706, 1066),
            }),
            frames,
            dropped: 0,
            first_frame_after: Some(Duration::from_millis(40)),
            gaps: Some((
                Duration::from_millis(15),
                Duration::from_millis(20),
                Duration::from_millis(17),
            )),
            observed_for: observed,
            niri_cast: Some(CastSummary {
                stream_id: 4,
                session_id: 2,
                target: "output eDP-1".to_string(),
                is_active: true,
            }),
            error: None,
        }
    }

    #[test]
    fn average_fps_needs_at_least_two_frames() {
        assert!(report(0, Duration::from_secs(5)).average_fps().is_none());
        assert!(report(1, Duration::from_secs(5)).average_fps().is_none());
        let fps = report(300, Duration::from_secs(5))
            .average_fps()
            .expect("two or more frames give a rate");
        assert!((fps - 60.0).abs() < 0.001, "{fps}");
    }

    #[test]
    fn the_report_says_what_the_encoder_would_be_told_and_that_nothing_was_written() {
        let text = report(300, Duration::from_secs(5)).to_string();
        assert!(text.contains("-pixel_format bgr0"), "{text}");
        assert!(text.contains("-video_size 2560x1600"), "{text}");
        assert!(text.contains("logical geometry: 1706x1066"), "{text}");
        assert!(text.contains("consumer attached: true"), "{text}");
        assert!(text.contains("nothing was written to disk"), "{text}");
    }

    #[test]
    fn the_report_is_honest_when_nothing_negotiated() {
        let mut failed = report(0, Duration::from_secs(5));
        failed.format = None;
        failed.niri_cast = None;
        failed.error = Some("no more input formats".to_string());
        let text = failed.to_string();
        assert!(text.contains("no format was ever agreed"), "{text}");
        assert!(text.contains("error: no more input formats"), "{text}");
    }

    // -- errors name the next thing to try -------------------------------

    #[test]
    fn cast_errors_are_actionable() {
        assert!(CastError::PipeWire("nope".to_string())
            .to_string()
            .contains("systemctl --user status pipewire"));
        assert!(CastError::ScreenCast {
            method: "CreateSession",
            err: "no such service".to_string(),
        }
        .to_string()
        .contains("busctl --user introspect"));
        assert!(CastError::ClosedEarly.to_string().contains("target"));
        assert!(CastError::NoNodeId.to_string().contains("PipeWire node"));
    }

    #[test]
    fn cast_target_displays_readably() {
        assert_eq!(
            CastTarget::Monitor {
                connector: "eDP-1".to_string()
            }
            .to_string(),
            "monitor eDP-1"
        );
        assert_eq!(CastTarget::Window { id: 19 }.to_string(), "window 19");
    }
}
