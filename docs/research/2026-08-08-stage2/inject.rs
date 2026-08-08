//! Stage-2 probe helper: inject pointer/keyboard input into a NESTED niri only.
//!
//! Uses zwlr_virtual_pointer_manager_v1 + zwp_virtual_keyboard_manager_v1 on whatever
//! WAYLAND_DISPLAY is set. It is bound to the nested compositor by the caller's env;
//! it never touches Jordan's real session (and the real niri does not even advertise
//! zwlr_virtual_pointer_manager_v1 to ordinary clients in this setup — see the probe log).
//!
//! usage: inject <width> <height>
//!   performs: move to (0.25w,0.25h), press left, 8 motions to (0.7w,0.65h), release,
//!             pause, then press+release Escape.

use std::os::fd::AsFd;

use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
};

#[derive(Default)]
struct App {
    seat: Option<wl_seat::WlSeat>,
    vp_mgr: Option<ZwlrVirtualPointerManagerV1>,
    vk_mgr: Option<ZwpVirtualKeyboardManagerV1>,
    keymap: Option<(u32, std::os::fd::OwnedFd, u32)>,
}

const BTN_LEFT: u32 = 0x110;
const KEY_ESC: u32 = 1; // evdev keycode

fn main() {
    let w: u32 = std::env::args().nth(1).unwrap().parse().unwrap();
    let h: u32 = std::env::args().nth(2).unwrap().parse().unwrap();

    let conn = Connection::connect_to_env().expect("connect");
    let mut q = conn.new_event_queue();
    let qh = q.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App::default();
    q.roundtrip(&mut app).unwrap();

    println!("inject: display={:?} {}x{}", std::env::var("WAYLAND_DISPLAY"), w, h);
    println!(
        "  virtual_pointer_manager={} virtual_keyboard_manager={}",
        app.vp_mgr.is_some(),
        app.vk_mgr.is_some()
    );
    let vp_mgr = app.vp_mgr.clone().expect("no zwlr_virtual_pointer_manager_v1");
    let seat = app.seat.clone();

    // grab the compositor's own keymap so the virtual keyboard speaks the same layout
    if let Some(s) = &seat {
        let _kb: wl_keyboard::WlKeyboard = s.get_keyboard(&qh, ());
        q.roundtrip(&mut app).unwrap();
    }

    let vp: ZwlrVirtualPointerV1 = vp_mgr.create_virtual_pointer(seat.as_ref(), &qh, ());

    let mut t: u32 = 1;
    let mut tick = || {
        t += 20;
        t
    };
    let x0 = w / 4;
    let y0 = h / 4;
    let x1 = w * 7 / 10;
    let y1 = h * 65 / 100;

    println!("  move to ({x0},{y0})");
    vp.motion_absolute(tick(), x0, y0, w, h);
    vp.frame();
    q.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));

    println!("  press left");
    vp.button(tick(), BTN_LEFT, wayland_client::protocol::wl_pointer::ButtonState::Pressed);
    vp.frame();
    q.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(120));

    for i in 1..=8 {
        let x = x0 + (x1 - x0) * i / 8;
        let y = y0 + (y1 - y0) * i / 8;
        println!("  motion {i} -> ({x},{y})");
        vp.motion_absolute(tick(), x, y, w, h);
        vp.frame();
        q.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(70));
    }

    println!("  release left");
    vp.button(tick(), BTN_LEFT, wayland_client::protocol::wl_pointer::ButtonState::Released);
    vp.frame();
    q.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));

    // Escape via a virtual keyboard sharing the compositor's keymap
    if std::env::args().nth(3).as_deref() == Some("nokey") { println!("  (skipping Escape)"); std::thread::sleep(std::time::Duration::from_millis(200)); q.flush().unwrap(); return; }
    match (&app.vk_mgr, &app.keymap, &seat) {
        (Some(mgr), Some((fmt, fd, size)), Some(s)) => {
            let vk: ZwpVirtualKeyboardV1 = mgr.create_virtual_keyboard(s, &qh, ());
            vk.keymap(*fmt, fd.as_fd(), *size);
            q.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(120));
            println!("  key ESC press");
            vk.key(tick(), KEY_ESC, 1);
            q.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(80));
            println!("  key ESC release");
            vk.key(tick(), KEY_ESC, 0);
            q.flush().unwrap();
        }
        _ => println!("  (no virtual keyboard / keymap; skipping Escape)"),
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    q.flush().unwrap();
    println!("inject: done");
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        st: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            match &interface[..] {
                "wl_seat" => st.seat = Some(r.bind(name, version.min(7), qh, ())),
                "zwlr_virtual_pointer_manager_v1" => {
                    st.vp_mgr = Some(r.bind(name, version, qh, ()))
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    st.vk_mgr = Some(r.bind(name, version, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        st: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = e {
            let f = match format {
                WEnum::Value(v) => v as u32,
                WEnum::Unknown(u) => u,
            };
            println!("  got compositor keymap: format={f} size={size}");
            st.keymap = Some((f, fd, size));
        }
    }
}

macro_rules! noop {
    ($t:ty) => {
        impl Dispatch<$t, ()> for App {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}
noop!(ZwlrVirtualPointerManagerV1);
noop!(ZwlrVirtualPointerV1);
noop!(ZwpVirtualKeyboardManagerV1);
noop!(ZwpVirtualKeyboardV1);

#[allow(unused)]
fn _unused(_: zwlr_virtual_pointer_v1::Event) {}
