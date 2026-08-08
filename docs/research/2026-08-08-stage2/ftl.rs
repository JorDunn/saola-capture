//! Stage-2 probe: ext_foreign_toplevel_list_v1 listing shape on niri 26.04.
//! Dumps every event each toplevel handle sends, in order, so we can see exactly
//! what identity/geometry information is (and is not) available.

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};

#[derive(Default)]
struct App {
    list: Option<ExtForeignToplevelListV1>,
    done: bool,
    n: usize,
}

fn main() {
    let conn = Connection::connect_to_env().expect("connect");
    let mut q = conn.new_event_queue();
    let qh = q.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App::default();
    q.roundtrip(&mut app).unwrap();

    match &app.list {
        Some(l) => println!(
            "== ext_foreign_toplevel_list_v1 bound, version {}",
            l.version()
        ),
        None => {
            println!("== ext_foreign_toplevel_list_v1 NOT ADVERTISED");
            return;
        }
    }
    println!("== events, verbatim, in arrival order:");
    for _ in 0..40 {
        q.blocking_dispatch(&mut app).unwrap();
        if app.done {
            break;
        }
    }
    println!("== {} toplevel handles announced; 'finished'/'done' seen = {}", app.n, app.done);
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
            if interface == "ext_foreign_toplevel_list_v1" {
                st.list = Some(r.bind(name, version, qh, ()));
            }
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for App {
    fn event(
        st: &mut Self,
        _: &ExtForeignToplevelListV1,
        e: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                st.n += 1;
                println!("  [list] toplevel -> new handle id {}", toplevel.id());
            }
            ext_foreign_toplevel_list_v1::Event::Finished => {
                println!("  [list] finished");
                st.done = true;
            }
            _ => {}
        }
    }

    // wayland-client needs an explicit specialization for events that CREATE new objects.
    // It must live INSIDE the impl Dispatch block (the macro expands to a bare fn).
    // Without it the dispatcher panics at runtime:
    //   "Missing event_created_child specialization for event opcode 0 of ext_foreign_toplevel_list_v1"
    wayland_client::event_created_child!(App, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for App {
    fn event(
        st: &mut Self,
        h: &ExtForeignToplevelHandleV1,
        e: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = h.id();
        match e {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                println!("     [handle {id}] identifier = {identifier:?}")
            }
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                println!("     [handle {id}] title      = {title:?}")
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                println!("     [handle {id}] app_id     = {app_id:?}")
            }
            ext_foreign_toplevel_handle_v1::Event::Done => {
                println!("     [handle {id}] done (atomic state applied)");
                st.done = true;
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                println!("     [handle {id}] closed")
            }
            other => println!("     [handle {id}] other event: {other:?}"),
        }
    }
}

