// ABOUTME: Declares AppKit view subclasses for the status item and popover.
// ABOUTME: Bridges telemetry data and mouse events into native macOS views.

use std::cell::RefCell;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::{ClassType, DeclaredClass, declare_class, msg_send, msg_send_id, mutability};
use objc2_app_kit::{NSBezierPath, NSColor, NSEvent, NSPopover, NSView};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSRectEdge, NSSize};
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
);

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
        unsafe { msg_send_id![super(this), initWithFrame: frame] }
    }

    pub fn attach_popover(&self, popover: Retained<NSPopover>, _mtm: MainThreadMarker) {
        *self.ivars().popover.borrow_mut() = Some(popover);
    }
}

fn draw_activity_sparklines(bounds: NSRect, frame: &TelemetryFrame) {
    let plot_bounds = centered_plot_bounds(bounds);
    let network_color = unsafe { NSColor::systemGreenColor() };
    draw_sparkline(
        plot_bounds,
        &frame.net_activity_history,
        &network_color,
        1.0,
    );

    let cpu_color = unsafe { NSColor::systemCyanColor() };
    draw_sparkline(plot_bounds, &frame.cpu_history, &cpu_color, 1.5);
}

fn centered_plot_bounds(bounds: NSRect) -> NSRect {
    let height = (bounds.size.height * 0.68).max(8.0);
    let origin_y = bounds.origin.y + (bounds.size.height - height) * 0.42;

    NSRect::new(
        NSPoint::new(bounds.origin.x, origin_y),
        NSSize::new(bounds.size.width, height),
    )
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

pub fn menu_bar_frame() -> NSRect {
    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(60.0, 22.0))
}
