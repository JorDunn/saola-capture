//! Stage-2 probe: is iced_layershell 0.19 viable for the region-selection overlay?
//!
//! Exercises, all at once, the four things PLAN.md says are unproven:
//!   1. per-output overlay surfaces          -> StartMode::AllScreens
//!   2. KeyboardInteractivity::Exclusive     -> and whether Escape actually arrives
//!   3. pointer drag fidelity on Layer::Overlay
//!   4. a frozen-frame image background at full output size
//!
//! Everything is logged to stdout. A hard watchdog exits after LIFETIME seconds so a
//! failure can never wedge the nested compositor.
//!
//! env: LSTEST_FRAME=/path/to/frozen.png  (optional background image)

use std::time::{Duration, Instant};

use iced::keyboard::key::Named;
use iced::widget::{canvas, container, image, stack, text, Column};
use iced::{event, keyboard, mouse, Color, Element, Length, Point, Rectangle, Subscription, Task};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::reexport::{NewLayerShellSettings, OutputOption};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;

const LIFETIME: u64 = 25;

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    Event(iced::Event),
    Tick,
}

struct Overlay {
    started: Instant,
    frame: Option<image::Handle>,
    cursor: Option<Point>,
    drag_from: Option<Point>,
    selection: Option<(Point, Point)>,
    log: Vec<String>,
    motions: usize,
    views_by_id: Vec<iced::window::Id>,
    escape_seen: bool,
}

impl Overlay {
    /// On-demand mode: boot with no layer surface, then spawn ONE overlay per named
    /// output via NewLayerShell{OutputOption::OutputName}. This is the shape Stage 6
    /// needs (the daemon is long-lived; the overlay only exists during a region shot).
    fn boot() -> (Self, Task<Message>) {
        let st = Self::new();
        let outputs = std::env::var("LSTEST_OUTPUTS").unwrap_or_default();
        let mut tasks = Vec::new();
        for name in outputs.split(',').filter(|s| !s.is_empty()) {
            println!("[boot] requesting on-demand overlay on output {name:?}");
            let settings = NewLayerShellSettings {
                size: Some((0, 0)),
                layer: Layer::Overlay,
                anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
                exclusive_zone: Some(-1),
                margin: Some((0, 0, 0, 0)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                output_option: OutputOption::OutputName(name.to_string()),
                events_transparent: false,
                namespace: Some("saola-capture-lstest-overlay".into()),
            };
            let (id, task) = Message::layershell_open(settings);
            println!("[boot] -> surface Id {id:?}");
            tasks.push(task);
        }
        (st, Task::batch(tasks))
    }

    fn new() -> Self {
        let frame = std::env::var("LSTEST_FRAME").ok().map(|p| {
            println!("[boot] loading frozen frame from {p}");
            image::Handle::from_path(p)
        });
        println!("[boot] frozen frame present: {}", frame.is_some());
        Self {
            started: Instant::now(),
            frame,
            cursor: None,
            drag_from: None,
            selection: None,
            log: Vec::new(),
            motions: 0,
            views_by_id: Vec::new(),
            escape_seen: false,
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                if self.started.elapsed() > Duration::from_secs(LIFETIME) {
                    println!("[watchdog] {LIFETIME}s elapsed; exiting");
                    self.report();
                    return iced::exit();
                }
                Task::none()
            }
            Message::Event(ev) => {
                match ev {
                    iced::Event::Keyboard(keyboard::Event::KeyPressed { ref key, .. }) => {
                        println!("[key] pressed {key:?}");
                        self.log.push(format!("key {key:?}"));
                        if matches!(key, keyboard::Key::Named(Named::Escape)) {
                            self.escape_seen = true;
                            println!("[key] ESCAPE received on an Exclusive-keyboard layer surface");
                            self.report();
                            return iced::exit();
                        }
                    }
                    iced::Event::Mouse(mouse::Event::ButtonPressed(b)) => {
                        println!("[mouse] press {b:?} at {:?}", self.cursor);
                        self.drag_from = self.cursor;
                        self.motions = 0;
                    }
                    iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                        self.cursor = Some(position);
                        if let Some(a) = self.drag_from {
                            self.motions += 1;
                            self.selection = Some((a, position));
                            if self.motions <= 5 || self.motions % 25 == 0 {
                                println!("[mouse] drag motion #{} -> {position:?}", self.motions);
                            }
                        }
                    }
                    iced::Event::Mouse(mouse::Event::ButtonReleased(b)) => {
                        println!(
                            "[mouse] release {b:?} at {:?}; drag had {} motion events; rect = {:?}",
                            self.cursor, self.motions, self.selection
                        );
                        self.drag_from = None;
                    }
                    _ => {}
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn report(&self) {
        println!("=== REPORT");
        println!("  surfaces this process rendered views for: {:?}", self.views_by_id);
        println!("  escape delivered to Exclusive surface: {}", self.escape_seen);
        println!("  last selection rect: {:?}", self.selection);
        println!("  key events seen: {}", self.log.len());
    }

    fn view(&self, id: iced::window::Id) -> Element<'_, Message> {
        // Not mutating self here (view takes &self), so surface accounting is printed instead.
        println!("[view] rendering surface {id:?}");
        let sel = canvas(SelectionCanvas {
            rect: self.selection,
        })
        .width(Length::Fill)
        .height(Length::Fill);

        let hud = Column::new()
            .push(text(format!("surface {id:?}")).size(28))
            .push(text(format!("cursor {:?}", self.cursor)).size(22))
            .push(text(format!("selection {:?}", self.selection)).size(22))
            .push(text("drag to select; Escape to quit").size(22));

        let base: Element<'_, Message> = match &self.frame {
            Some(h) => image(h.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::Fill)
                .into(),
            None => container(text("")).width(Length::Fill).height(Length::Fill).into(),
        };

        stack![base, sel, container(hud).padding(40)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            event::listen().map(Message::Event),
            iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
        ])
    }
}

struct SelectionCanvas {
    rect: Option<(Point, Point)>,
}

impl<M> canvas::Program<M> for SelectionCanvas {
    type State = ();
    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut f = canvas::Frame::new(renderer, bounds.size());
        // scrim
        f.fill_rectangle(
            Point::ORIGIN,
            bounds.size(),
            Color::from_rgba(0.05, 0.04, 0.0, 0.45),
        );
        if let Some((a, b)) = self.rect {
            let x = a.x.min(b.x);
            let y = a.y.min(b.y);
            let w = (b.x - a.x).abs();
            let h = (b.y - a.y).abs();
            f.fill_rectangle(
                Point::new(x, y),
                iced::Size::new(w, h),
                Color::from_rgba(0.0, 0.0, 0.0, 0.0),
            );
            f.stroke_rectangle(
                Point::new(x, y),
                iced::Size::new(w, h),
                canvas::Stroke::default()
                    .with_color(Color::from_rgb(0.78, 0.40, 0.27))
                    .with_width(3.0),
            );
        }
        vec![f.into_geometry()]
    }
}

fn main() -> iced_layershell::Result {
    println!("== lstest: iced_layershell overlay viability probe");
    println!("   WAYLAND_DISPLAY = {:?}", std::env::var("WAYLAND_DISPLAY"));
    let ondemand = std::env::var("LSTEST_OUTPUTS").is_ok();
    let start_mode = if ondemand { StartMode::Background } else { StartMode::AllScreens };
    println!("   start_mode = {}", if ondemand { "Background + on-demand NewLayerShell" } else { "AllScreens" });
    daemon(Overlay::boot, "saola-capture-lstest", Overlay::update, Overlay::view)
        .subscription(Overlay::subscription)
        .settings(Settings {
            layer_settings: LayerShellSettings {
                anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
                layer: Layer::Overlay,
                exclusive_zone: -1,
                size: Some((0, 0)),
                margin: (0, 0, 0, 0),
                events_transparent: false,
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                // THE per-output question: one surface per wl_output.
                start_mode,
            },
            ..Default::default()
        })
        .run()
}
