// ABOUTME: Declares AppKit view subclasses for the status item and popover.
// ABOUTME: Bridges telemetry data and mouse events into native macOS views.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, DeclaredClass, declare_class, msg_send, msg_send_id, mutability};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSEvent, NSPopover, NSToolTipTag, NSView, NSViewToolTipOwner,
};
use objc2_foundation::{
    MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSRectEdge, NSSize, NSString,
};
use objc2_quartz_core::CAMetalLayer;
use parking_lot::Mutex;
use triple_buffer::Output;

use crate::gpu::GpuEngine;
use crate::telemetry::TelemetryFrame;

pub struct CanvasIvars {
    pub gpu_engine: Mutex<Option<GpuEngine<'static>>>,
    pub pending_events: Mutex<Vec<egui::Event>>,
    pub telemetry: Arc<Mutex<Output<TelemetryFrame>>>,
}

pub struct MenuBarIvars {
    pub telemetry: Arc<Mutex<Output<TelemetryFrame>>>,
    pub popover: RefCell<Option<Retained<NSPopover>>>,
}

struct MenuBarItemRects {
    panel: NSRect,
    cpu: NSRect,
    memory: NSRect,
    network: NSRect,
    temperature: NSRect,
}

#[derive(Clone, Copy)]
enum TooltipMetric {
    Cpu = 1,
    Memory = 2,
    Network = 3,
    Temperature = 4,
}

declare_class!(
    pub struct AetherCanvasView;

    // SAFETY: NSView supports subclassing and all AppKit callbacks run on the main thread.
    unsafe impl ClassType for AetherCanvasView {
        type Super = NSView;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "AetherCanvasView";
    }

    impl DeclaredClass for AetherCanvasView {
        type Ivars = CanvasIvars;
    }

    unsafe impl AetherCanvasView {
        #[method_id(makeBackingLayer)]
        fn make_backing_layer(&self) -> Retained<CAMetalLayer> {
            unsafe { msg_send_id![CAMetalLayer::class(), layer] }
        }

        #[method(viewDidMoveToWindow)]
        fn view_did_move_to_window(&self) {
            let _mtm = MainThreadMarker::from(self);
            let _: () = unsafe { msg_send![self, setWantsLayer: true] };

            let layer_ptr: *mut std::ffi::c_void = unsafe { msg_send![self, layer] };
            if layer_ptr.is_null() {
                return;
            }
            let bounds: NSRect = unsafe { msg_send![self, bounds] };

            let engine = unsafe {
                GpuEngine::new_from_metal_layer(
                    layer_ptr,
                    bounds.size.width as u32,
                    bounds.size.height as u32,
                )
            };

            match engine {
                Ok(engine) => {
                    *self.ivars().gpu_engine.lock() = Some(engine);
                    self.render_current_frame();
                }
                Err(error) => {
                    eprintln!("AetherCanvasView GPU init failed: {error}");
                }
            }
        }

        #[method(mouseDown:)]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convert_point(event);
            self.dispatch_egui_event(egui::Event::PointerButton {
                pos: egui::pos2(point.x as f32, point.y as f32),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            });
            self.render_current_frame();
        }

        #[method(mouseUp:)]
        fn mouse_up(&self, event: &NSEvent) {
            let point = self.convert_point(event);
            self.dispatch_egui_event(egui::Event::PointerButton {
                pos: egui::pos2(point.x as f32, point.y as f32),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            });
            self.render_current_frame();
        }

        #[method(drawRect:)]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            self.render_current_frame();
        }

        #[method(renderTelemetry)]
        fn render_telemetry(&self) {
            self.render_current_frame();
        }
    }
);

declare_class!(
    pub struct AetherMenuBarView;

    // SAFETY: NSView supports subclassing and the telemetry consumer is protected by a mutex.
    unsafe impl ClassType for AetherMenuBarView {
        type Super = NSView;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "AetherMenuBarView";
    }

    impl DeclaredClass for AetherMenuBarView {
        type Ivars = MenuBarIvars;
    }

    unsafe impl AetherMenuBarView {
        #[method(drawRect:)]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let _mtm = MainThreadMarker::from(self);
            let frame = *self.ivars().telemetry.lock().read();
            let bounds: NSRect = unsafe { msg_send![self, bounds] };
            draw_activity_sparklines(bounds, &frame);
        }

        #[method(redrawTelemetry)]
        fn redraw_telemetry(&self) {
            let _mtm = MainThreadMarker::from(self);
            let _: () = unsafe { msg_send![self, setNeedsDisplay: true] };
        }

        #[method(mouseDown:)]
        fn mouse_down(&self, _event: &NSEvent) {
            let _mtm = MainThreadMarker::from(self);
            let Some(popover) = self.ivars().popover.borrow().clone() else {
                return;
            };
            let bounds: NSRect = unsafe { msg_send![self, bounds] };
            let view: &NSView = self.as_ref();
            unsafe {
                popover.showRelativeToRect_ofView_preferredEdge(
                    bounds,
                    view,
                    NSRectEdge::NSMinYEdge,
                )
            };
        }
    }

    unsafe impl NSViewToolTipOwner for AetherMenuBarView {
        #[method_id(view:stringForToolTip:point:userData:)]
        fn view_string_for_tool_tip(
            &self,
            _view: &NSView,
            _tag: NSToolTipTag,
            _point: NSPoint,
            data: *mut c_void,
        ) -> Retained<NSString> {
            let frame = *self.ivars().telemetry.lock().read();
            NSString::from_str(&tooltip_text(tooltip_metric_from_data(data), &frame))
        }
    }
);

unsafe impl NSObjectProtocol for AetherMenuBarView {}

impl AetherCanvasView {
    pub fn new(
        frame: NSRect,
        telemetry: Arc<Mutex<Output<TelemetryFrame>>>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(CanvasIvars {
            gpu_engine: Mutex::new(None),
            pending_events: Mutex::new(Vec::new()),
            telemetry,
        });
        unsafe { msg_send_id![super(this), initWithFrame: frame] }
    }

    fn convert_point(&self, event: &NSEvent) -> NSPoint {
        let window_point = unsafe { event.locationInWindow() };
        let view_point: NSPoint = unsafe {
            msg_send![self, convertPoint: window_point fromView: std::ptr::null::<NSView>()]
        };
        let bounds: NSRect = unsafe { msg_send![self, bounds] };
        NSPoint::new(view_point.x, bounds.size.height - view_point.y)
    }

    fn dispatch_egui_event(&self, event: egui::Event) {
        self.ivars().pending_events.lock().push(event);
    }

    fn render_current_frame(&self) {
        let _mtm = MainThreadMarker::from(self);
        let frame = *self.ivars().telemetry.lock().read();
        let bounds: NSRect = unsafe { msg_send![self, bounds] };

        if let Some(engine) = self.ivars().gpu_engine.lock().as_mut() {
            // Hand egui the buffered pointer events; only drain once an engine
            // exists so input arriving before setup is not silently discarded.
            let events = std::mem::take(&mut *self.ivars().pending_events.lock());
            engine.resize(bounds.size.width as u32, bounds.size.height as u32);
            if let Err(error) = engine.render(&frame, events) {
                eprintln!("AetherCanvasView render failed: {error:?}");
            }
        }
    }
}

impl AetherMenuBarView {
    pub fn new(
        frame: NSRect,
        telemetry: Arc<Mutex<Output<TelemetryFrame>>>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(MenuBarIvars {
            telemetry,
            popover: RefCell::new(None),
        });
        let view: Retained<Self> = unsafe { msg_send_id![super(this), initWithFrame: frame] };
        view.install_tooltips();
        view
    }

    pub fn attach_popover(&self, popover: Retained<NSPopover>, _mtm: MainThreadMarker) {
        *self.ivars().popover.borrow_mut() = Some(popover);
    }

    fn install_tooltips(&self) {
        let bounds: NSRect = unsafe { msg_send![self, bounds] };
        let rects = menu_bar_item_rects(bounds);
        let owner: &AnyObject = self.as_ref();

        unsafe { self.removeAllToolTips() };
        add_tooltip_rect(self, rects.cpu, owner, TooltipMetric::Cpu);
        add_tooltip_rect(self, rects.memory, owner, TooltipMetric::Memory);
        add_tooltip_rect(self, rects.network, owner, TooltipMetric::Network);
        add_tooltip_rect(self, rects.temperature, owner, TooltipMetric::Temperature);
    }
}

fn draw_activity_sparklines(bounds: NSRect, frame: &TelemetryFrame) {
    let rects = menu_bar_item_rects(bounds);
    fill_rounded_rect(rects.panel, 4.5, &color(0.045, 0.051, 0.059, 0.96));
    stroke_rounded_rect(rects.panel, 4.5, &color(0.24, 0.27, 0.30, 0.72), 0.6);

    draw_divider(rects.cpu.origin.x + rects.cpu.size.width + 3.5, rects.panel);
    draw_divider(
        rects.memory.origin.x + rects.memory.size.width + 3.5,
        rects.panel,
    );
    draw_divider(
        rects.network.origin.x + rects.network.size.width + 3.5,
        rects.panel,
    );

    draw_cpu_bars(rects.cpu, &frame.cpu_history);
    draw_level_meter(
        rects.memory,
        memory_ratio_value(frame),
        &color(0.45, 0.63, 1.0, 0.92),
    );
    draw_sparkline(
        rects.network,
        &frame.net_activity_history,
        &color(0.14, 0.91, 0.63, 0.96),
        1.05,
    );
    draw_temperature_gauge(rects.temperature, frame.temp_celsius);
}

fn draw_sparkline(bounds: NSRect, values: &[f32; 60], color: &NSColor, line_width: f64) {
    let path = unsafe { NSBezierPath::bezierPath() };
    unsafe { path.setLineWidth(line_width) };

    let width = bounds.size.width.max(1.0);
    let height = bounds.size.height.max(1.0);
    let step = width / 59.0;

    for (index, value) in values.iter().enumerate() {
        let x = bounds.origin.x + index as f64 * step;
        let y = bounds.origin.y + (f64::from(*value).clamp(0.0, 100.0) / 100.0 * height);
        let point = NSPoint::new(x, y);

        if index == 0 {
            unsafe { path.moveToPoint(point) };
        } else {
            unsafe { path.lineToPoint(point) };
        }
    }

    unsafe { color.setStroke() };
    unsafe { path.stroke() };
}

fn draw_cpu_bars(bounds: NSRect, values: &[f32; 60]) {
    let slots = 8;
    let gap = 1.2;
    let bar_width = ((bounds.size.width - gap * (slots as f64 - 1.0)) / slots as f64).max(1.0);

    for index in 0..slots {
        let value = values[values.len() - slots + index].clamp(0.0, 100.0);
        let height = (bounds.size.height * f64::from(value) / 100.0).max(1.0);
        let rect = NSRect::new(
            NSPoint::new(
                bounds.origin.x + index as f64 * (bar_width + gap),
                bounds.origin.y,
            ),
            NSSize::new(bar_width, height),
        );
        fill_rounded_rect(rect, 0.9, &color(0.35, 0.88, 1.0, 0.85));
    }
}

fn draw_level_meter(bounds: NSRect, ratio: f32, fill_color: &NSColor) {
    fill_rounded_rect(bounds, 1.4, &color(0.12, 0.14, 0.16, 0.95));
    let fill_width = bounds.size.width * f64::from(ratio.clamp(0.0, 1.0));
    let fill_rect = NSRect::new(bounds.origin, NSSize::new(fill_width, bounds.size.height));
    fill_rounded_rect(fill_rect, 1.4, fill_color);
    stroke_rounded_rect(bounds, 1.4, &color(0.37, 0.40, 0.44, 0.62), 0.5);
}

fn draw_temperature_gauge(bounds: NSRect, temperature: f32) {
    let ratio = (temperature / 100.0).clamp(0.0, 1.0);
    let fill_color = if temperature >= 80.0 {
        color(1.0, 0.28, 0.22, 0.95)
    } else if temperature >= 60.0 {
        color(1.0, 0.68, 0.20, 0.95)
    } else {
        color(0.71, 0.93, 0.35, 0.95)
    };
    let bulb_size = bounds.size.height.min(8.0);
    let bulb_rect = NSRect::new(
        NSPoint::new(bounds.origin.x, bounds.origin.y),
        NSSize::new(bulb_size, bulb_size),
    );
    fill_rounded_rect(bulb_rect, bulb_size / 2.0, &fill_color);

    let stem = NSRect::new(
        NSPoint::new(bounds.origin.x + bulb_size + 3.0, bounds.origin.y + 2.0),
        NSSize::new(
            bounds.size.width - bulb_size - 3.0,
            bounds.size.height - 4.0,
        ),
    );
    draw_level_meter(stem, ratio, &fill_color);
}

fn draw_divider(x: f64, panel: NSRect) {
    let path = unsafe { NSBezierPath::bezierPath() };
    unsafe { path.setLineWidth(0.6) };
    unsafe { path.moveToPoint(NSPoint::new(x, panel.origin.y + 3.0)) };
    unsafe { path.lineToPoint(NSPoint::new(x, panel.origin.y + panel.size.height - 3.0)) };
    unsafe { color(0.34, 0.37, 0.40, 0.55).setStroke() };
    unsafe { path.stroke() };
}

fn fill_rounded_rect(rect: NSRect, radius: f64, color: &NSColor) {
    let path =
        unsafe { NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius) };
    unsafe { color.setFill() };
    unsafe { path.fill() };
}

fn stroke_rounded_rect(rect: NSRect, radius: f64, color: &NSColor, line_width: f64) {
    let path =
        unsafe { NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius) };
    unsafe { path.setLineWidth(line_width) };
    unsafe { color.setStroke() };
    unsafe { path.stroke() };
}

fn inset_rect(rect: NSRect, x: f64, y: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.origin.x + x, rect.origin.y + y),
        NSSize::new(
            (rect.size.width - x * 2.0).max(0.0),
            (rect.size.height - y * 2.0).max(0.0),
        ),
    )
}

fn color(red: f64, green: f64, blue: f64, alpha: f64) -> Retained<NSColor> {
    unsafe { NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, alpha) }
}

fn memory_ratio_value(frame: &TelemetryFrame) -> f32 {
    if frame.mem_total_mb == 0 {
        return 0.0;
    }

    (frame.mem_used_mb as f32 / frame.mem_total_mb as f32).clamp(0.0, 1.0)
}

fn menu_bar_item_rects(bounds: NSRect) -> MenuBarItemRects {
    let panel = inset_rect(bounds, 1.0, 2.0);
    let cpu = NSRect::new(
        NSPoint::new(panel.origin.x + 5.0, panel.origin.y + 4.0),
        NSSize::new(25.0, panel.size.height - 8.0),
    );
    let memory = NSRect::new(
        NSPoint::new(cpu.origin.x + cpu.size.width + 7.0, panel.origin.y + 4.0),
        NSSize::new(18.0, panel.size.height - 8.0),
    );
    let network = NSRect::new(
        NSPoint::new(
            memory.origin.x + memory.size.width + 7.0,
            panel.origin.y + 4.0,
        ),
        NSSize::new(43.0, panel.size.height - 8.0),
    );
    let temperature = NSRect::new(
        NSPoint::new(
            network.origin.x + network.size.width + 7.0,
            panel.origin.y + 4.0,
        ),
        NSSize::new(21.0, panel.size.height - 8.0),
    );

    MenuBarItemRects {
        panel,
        cpu,
        memory,
        network,
        temperature,
    }
}

fn add_tooltip_rect(
    view: &AetherMenuBarView,
    rect: NSRect,
    owner: &AnyObject,
    metric: TooltipMetric,
) {
    let data = metric as usize as *mut c_void;
    unsafe {
        view.addToolTipRect_owner_userData(rect, owner, data);
    }
}

fn tooltip_metric_from_data(data: *mut c_void) -> TooltipMetric {
    match data as usize {
        1 => TooltipMetric::Cpu,
        2 => TooltipMetric::Memory,
        3 => TooltipMetric::Network,
        4 => TooltipMetric::Temperature,
        _ => TooltipMetric::Cpu,
    }
}

fn tooltip_text(metric: TooltipMetric, frame: &TelemetryFrame) -> String {
    match metric {
        TooltipMetric::Cpu => format!("CPU {:.1}%", frame.cpu_total),
        TooltipMetric::Memory => format!(
            "Memory {} MB / {} MB ({:.0}%)",
            frame.mem_used_mb,
            frame.mem_total_mb,
            memory_ratio_value(frame) * 100.0
        ),
        TooltipMetric::Network => format!(
            "Network in {}  out {}",
            compact_bytes_per_second(frame.net_in_bytes_sec),
            compact_bytes_per_second(frame.net_out_bytes_sec)
        ),
        TooltipMetric::Temperature => format!("Temperature {:.1} C", frame.temp_celsius),
    }
}

fn compact_bytes_per_second(bytes_per_sec: u64) -> String {
    if bytes_per_sec >= 1_000_000 {
        return format!("{:.1} MB/s", bytes_per_sec as f32 / 1_000_000.0);
    }
    if bytes_per_sec >= 1_000 {
        return format!("{:.1} KB/s", bytes_per_sec as f32 / 1_000.0);
    }

    format!("{bytes_per_sec} B/s")
}

pub fn menu_bar_frame() -> NSRect {
    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(136.0, 22.0))
}
