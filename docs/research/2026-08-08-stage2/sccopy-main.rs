//! Stage-2 screencopy handshake probe. Throwaway; lives in the scratch dir.
//!
//! Logs every event zwlr_screencopy_frame_v1 sends, for both capture_output and
//! capture_output_region, with cursor on and off, and dumps pixels so orientation
//! and channel order can be checked against grim.
//!
//! usage: sccopy [output|region] [cursor|nocursor]

use std::os::fd::AsFd;
use std::os::fd::FromRawFd as _;

use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

#[derive(Default)]
struct Outputs {
    // (wl_output, name, logical/mode info gathered from wl_output events)
    list: Vec<(wl_output::WlOutput, String, i32, i32, i32, i32)>, // obj, name, mode_w, mode_h, scale, transform
}

struct App {
    shm: Option<wl_shm::WlShm>,
    manager: Option<ZwlrScreencopyManagerV1>,
    outputs: Outputs,
    shm_formats: Vec<u32>,
    // frame handshake state
    buffer_offers: Vec<(u32, u32, u32, u32)>,      // format, width, height, stride
    dmabuf_offers: Vec<(u32, u32, u32)>,           // fourcc, width, height
    buffer_done: bool,
    flags: Option<u32>,
    ready: bool,
    failed: bool,
    damage: Vec<(u32, u32, u32, u32)>,
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "output".into());
    let cursor = std::env::args().nth(2).unwrap_or_else(|| "nocursor".into());
    let overlay_cursor: i32 = if cursor == "cursor" { 1 } else { 0 };

    let conn = Connection::connect_to_env().expect("connect");
    let display = conn.display();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    display.get_registry(&qh, ());

    let mut app = App {
        shm: None,
        manager: None,
        outputs: Outputs::default(),
        shm_formats: Vec::new(),
        buffer_offers: Vec::new(),
        dmabuf_offers: Vec::new(),
        buffer_done: false,
        flags: None,
        ready: false,
        failed: false,
        damage: Vec::new(),
    };

    queue.roundtrip(&mut app).unwrap();
    queue.roundtrip(&mut app).unwrap(); // outputs' own events

    println!("== globals bound");
    println!(
        "   zwlr_screencopy_manager_v1 version = {:?}",
        app.manager.as_ref().map(|m| m.version())
    );
    println!("   wl_shm formats advertised by the compositor (wl_shm.format events):");
    for f in &app.shm_formats {
        println!(
            "      {} ({})",
            f,
            match f {
                0 => "Argb8888",
                1 => "Xrgb8888",
                other => {
                    // fourcc for everything else
                    let b = other.to_le_bytes();
                    return_fourcc(b)
                }
            }
        );
    }
    println!("== outputs:");
    for (_, name, w, h, s, t) in &app.outputs.list {
        println!("   {name}: mode {w}x{h} scale {s} transform {t}");
    }

    let (output, name, _mode_w, _mode_h, ..) = app.outputs.list[0].clone();
    let manager = app.manager.clone().unwrap();

    println!("\n== requesting capture, mode={mode} overlay_cursor={overlay_cursor} on {name}");
    let _frame: ZwlrScreencopyFrameV1 = if mode == "region" {
        // region args are in *logical* coordinates per the protocol; niri scales them.
        manager.capture_output_region(overlay_cursor, &output, 100, 100, 400, 300, &qh, ())
    } else {
        manager.capture_output(overlay_cursor, &output, &qh, ())
    };

    // Pump until buffer_done (v3) or first buffer (v1/v2).
    for _ in 0..50 {
        queue.blocking_dispatch(&mut app).unwrap();
        if app.buffer_done || app.failed {
            break;
        }
    }

    println!("== handshake events received BEFORE we attach a buffer:");
    for (f, w, h, s) in &app.buffer_offers {
        println!(
            "   buffer(format={} [{}], width={}, height={}, stride={})",
            f,
            match f {
                0 => "Argb8888".to_string(),
                1 => "Xrgb8888".to_string(),
                other => return_fourcc(other.to_le_bytes()).to_string(),
            },
            w,
            h,
            s
        );
    }
    for (fourcc, w, h) in &app.dmabuf_offers {
        println!(
            "   linux_dmabuf(fourcc=0x{:08x} '{}', width={}, height={})",
            fourcc,
            return_fourcc(fourcc.to_le_bytes()),
            w,
            h
        );
    }
    println!("   buffer_done = {}", app.buffer_done);
    println!("   failed      = {}", app.failed);
    if app.failed {
        return;
    }

    // Attach an shm buffer using the FIRST advertised shm buffer offer.
    let (fmt, w, h, stride) = app.buffer_offers[0];
    let len = (stride * h) as usize;
    println!("\n== allocating shm buffer {w}x{h} stride={stride} len={len} format={fmt}");

    let raw = unsafe { libc::memfd_create(c"sccopy".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(raw >= 0, "memfd_create failed");
    assert_eq!(unsafe { libc::ftruncate(raw, len as libc::off_t) }, 0);
    let fd = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(raw) };
    let mut map = unsafe { memmap2::MmapMut::map_mut(&fd).unwrap() };
    // poison the buffer so we can tell written-vs-untouched rows apart
    map.fill(0x7f);

    let shm = app.shm.clone().unwrap();
    let pool: wl_shm_pool::WlShmPool = shm.create_pool(fd.as_fd(), len as i32, &qh, ());
    let buffer: wl_buffer::WlBuffer = pool.create_buffer(
        0,
        w as i32,
        h as i32,
        stride as i32,
        wl_shm::Format::try_from(fmt).unwrap(),
        &qh,
        (),
    );

    _frame.copy(&buffer);
    for _ in 0..200 {
        queue.blocking_dispatch(&mut app).unwrap();
        if app.ready || app.failed {
            break;
        }
    }
    println!("== after copy():");
    println!("   flags  = {:?}  (1 = y_invert)", app.flags);
    println!("   damage events = {:?}", app.damage);
    println!("   ready  = {}", app.ready);
    println!("   failed = {}", app.failed);
    if !app.ready {
        return;
    }

    // Pixel report: XRGB8888 little-endian == bytes B,G,R,X in memory.
    let px = |x: u32, y: u32| -> (u8, u8, u8, u8) {
        let o = (y * stride + x * 4) as usize;
        (map[o], map[o + 1], map[o + 2], map[o + 3])
    };
    println!("\n== pixels (bytes as stored, i.e. B G R X for wl_shm Xrgb8888 on LE):");
    println!("   (0,0)      = {:?}", px(0, 0));
    println!("   (1,0)      = {:?}", px(1, 0));
    println!("   (w/2,h/2)  = {:?}", px(w / 2, h / 2));
    println!("   (0,h-1)    = {:?}", px(0, h - 1));
    let untouched = map.iter().filter(|&&b| b == 0x7f).count();
    println!(
        "   bytes still 0x7f (never written) = {untouched} / {len} ({:.2}%)",
        untouched as f64 * 100.0 / len as f64
    );

    // dump raw for external comparison
    let out = format!("/tmp/claude-1000/-home-jordan-Developer-saola-capture/23479e85-b6aa-4c8e-a055-02ab53cdd73a/scratchpad/sccopy-{mode}-{cursor}.raw");
    std::fs::write(&out, &map[..]).unwrap();
    println!("   wrote {out} ({w}x{h} stride {stride})");

    buffer.destroy();
    pool.destroy();
}

fn return_fourcc(b: [u8; 4]) -> &'static str {
    // leak a tiny string; this is a throwaway probe
    Box::leak(
        String::from_utf8_lossy(&b)
            .chars()
            .filter(|c| c.is_ascii_graphic())
            .collect::<String>()
            .into_boxed_str(),
    )
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        st: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match &interface[..] {
                "wl_shm" => st.shm = Some(registry.bind(name, version.min(1), qh, ())),
                "zwlr_screencopy_manager_v1" => {
                    st.manager = Some(registry.bind(name, version, qh, ()))
                }
                "wl_output" => {
                    let o: wl_output::WlOutput = registry.bind(name, version.min(4), qh, ());
                    st.outputs.list.push((o, String::new(), 0, 0, 1, 0));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for App {
    fn event(
        st: &mut Self,
        _: &wl_shm::WlShm,
        event: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_shm::Event::Format { format } = event {
            if let WEnum::Value(f) = format {
                st.shm_formats.push(f as u32);
            } else if let WEnum::Unknown(u) = format {
                st.shm_formats.push(u);
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for App {
    fn event(
        st: &mut Self,
        obj: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(e) = st.outputs.list.iter_mut().find(|(o, ..)| o == obj) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => e.1 = name,
            wl_output::Event::Mode { width, height, .. } => {
                e.2 = width;
                e.3 = height;
            }
            wl_output::Event::Scale { factor } => e.4 = factor,
            wl_output::Event::Geometry { transform, .. } => {
                if let WEnum::Value(t) = transform {
                    e.5 = t as i32;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZwlrScreencopyManagerV1,
        _: <ZwlrScreencopyManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for App {
    fn event(
        st: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event as E;
        match event {
            E::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                let f = match format {
                    WEnum::Value(v) => v as u32,
                    WEnum::Unknown(u) => u,
                };
                println!("   [event] buffer format={f} {width}x{height} stride={stride}");
                st.buffer_offers.push((f, width, height, stride));
            }
            E::LinuxDmabuf {
                format,
                width,
                height,
            } => {
                println!("   [event] linux_dmabuf fourcc=0x{format:08x} {width}x{height}");
                st.dmabuf_offers.push((format, width, height));
            }
            E::BufferDone => {
                println!("   [event] buffer_done");
                st.buffer_done = true;
            }
            E::Flags { flags } => {
                let f = match flags {
                    WEnum::Value(v) => v.bits(),
                    WEnum::Unknown(u) => u,
                };
                println!("   [event] flags = {f}");
                st.flags = Some(f);
            }
            E::Damage {
                x,
                y,
                width,
                height,
            } => {
                println!("   [event] damage {x},{y} {width}x{height}");
                st.damage.push((x, y, width, height));
            }
            E::Ready { .. } => {
                println!("   [event] ready");
                st.ready = true;
            }
            E::Failed => {
                println!("   [event] failed");
                st.failed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
