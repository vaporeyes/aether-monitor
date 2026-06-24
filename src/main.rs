// ABOUTME: Starts the Aether Monitor native status bar application.
// ABOUTME: Wires telemetry polling into AppKit status item and popover views.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use aether_monitor::telemetry::{TelemetryFrame, TelemetryPipe};
use aether_monitor::ui::{AetherCanvasView, AetherMenuBarView, menu_bar_frame};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::sel;
use objc2::{ClassType, DeclaredClass, declare_class, msg_send, msg_send_id, mutability};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSPopover,
    NSPopoverBehavior, NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSView,
    NSViewController,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
};
use parking_lot::Mutex;
use triple_buffer::Output;

static MENU_VIEW_PTR: AtomicUsize = AtomicUsize::new(0);
static CANVAS_VIEW_PTR: AtomicUsize = AtomicUsize::new(0);

struct AppDelegateIvars {
    telemetry: Arc<Mutex<Output<TelemetryFrame>>>,
    status_item: RefCell<Option<Retained<NSStatusItem>>>,
    menu_view: RefCell<Option<Retained<AetherMenuBarView>>>,
    popover: RefCell<Option<Retained<NSPopover>>>,
    canvas_view: RefCell<Option<Retained<AetherCanvasView>>>,
    view_controller: RefCell<Option<Retained<NSViewController>>>,
}

declare_class!(
    struct AppDelegate;

    // SAFETY: NSObject supports subclassing and this delegate is only used on the main thread.
    unsafe impl ClassType for AppDelegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "AetherAppDelegate";
    }

    impl DeclaredClass for AppDelegate {
        type Ivars = AppDelegateIvars;
    }

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[method(applicationDidFinishLaunching:)]
        fn application_did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::from(self);
            self.build_status_ui(mtm);
        }
    }
);

unsafe impl NSObjectProtocol for AppDelegate {}

impl AppDelegate {
    fn new(telemetry: Arc<Mutex<Output<TelemetryFrame>>>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(AppDelegateIvars {
            telemetry,
            status_item: RefCell::new(None),
            menu_view: RefCell::new(None),
            popover: RefCell::new(None),
            canvas_view: RefCell::new(None),
            view_controller: RefCell::new(None),
        });
        unsafe { msg_send_id![super(this), init] }
    }

    fn build_status_ui(&self, mtm: MainThreadMarker) {
        let status_bar = unsafe { NSStatusBar::systemStatusBar() };
        let status_item = unsafe { status_bar.statusItemWithLength(NSVariableStatusItemLength) };
        let menu_view =
            AetherMenuBarView::new(menu_bar_frame(), self.ivars().telemetry.clone(), mtm);

        let popover = unsafe { NSPopover::init(mtm.alloc::<NSPopover>()) };
        unsafe { popover.setBehavior(NSPopoverBehavior::Transient) };
        unsafe { popover.setContentSize(NSSize::new(360.0, 240.0)) };

        let content_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(360.0, 240.0));
        let canvas_view = AetherCanvasView::new(content_frame, self.ivars().telemetry.clone(), mtm);
        let view_controller = unsafe { NSViewController::init(mtm.alloc::<NSViewController>()) };
        let canvas_as_view: &NSView = canvas_view.as_ref();
        unsafe { view_controller.setView(canvas_as_view) };
        unsafe { popover.setContentViewController(Some(&view_controller)) };
        CANVAS_VIEW_PTR.store(Retained::as_ptr(&canvas_view) as usize, Ordering::Release);

        menu_view.attach_popover(popover.clone(), mtm);
        let menu_as_view: &NSView = menu_view.as_ref();
        let _: () = unsafe { msg_send![&*status_item, setView: Some(menu_as_view)] };
        MENU_VIEW_PTR.store(Retained::as_ptr(&menu_view) as usize, Ordering::Release);

        *self.ivars().status_item.borrow_mut() = Some(status_item);
        *self.ivars().menu_view.borrow_mut() = Some(menu_view);
        *self.ivars().popover.borrow_mut() = Some(popover);
        *self.ivars().canvas_view.borrow_mut() = Some(canvas_view);
        *self.ivars().view_controller.borrow_mut() = Some(view_controller);
    }
}

fn main() {
    let (mut pipe, consumer) = TelemetryPipe::new();
    thread::spawn(move || {
        let mut system = sysinfo::System::new_all();
        let mut networks = sysinfo::Networks::new_with_refreshed_list();
        let mut components = sysinfo::Components::new_with_refreshed_list();
        let mut cpu_history = [0.0; 60];
        let mut net_activity_history = [0.0; 60];

        loop {
            thread::sleep(Duration::from_millis(1000));
            system.refresh_all();
            networks.refresh();
            components.refresh();
            cpu_history.rotate_left(1);
            cpu_history[59] = system.global_cpu_info().cpu_usage();
            let net_in_bytes_sec: u64 = networks.values().map(|network| network.received()).sum();
            let net_out_bytes_sec: u64 =
                networks.values().map(|network| network.transmitted()).sum();
            net_activity_history.rotate_left(1);
            net_activity_history[59] =
                network_activity_percent(net_in_bytes_sec.saturating_add(net_out_bytes_sec));

            pipe.push(TelemetryFrame {
                cpu_total: cpu_history[59],
                cpu_history,
                net_activity_history,
                mem_used_mb: system.used_memory() / 1024 / 1024,
                mem_total_mb: system.total_memory() / 1024 / 1024,
                net_in_bytes_sec,
                net_out_bytes_sec,
                temp_celsius: max_component_temperature(&components),
            });

            let menu_view_ptr = MENU_VIEW_PTR.load(Ordering::Acquire);
            if menu_view_ptr != 0 {
                let menu_view = menu_view_ptr as *mut AnyObject;
                let _: () = unsafe {
                    msg_send![
                        menu_view,
                        performSelectorOnMainThread: sel!(redrawTelemetry)
                        withObject: std::ptr::null_mut::<AnyObject>()
                        waitUntilDone: false
                    ]
                };
            }

            let canvas_view_ptr = CANVAS_VIEW_PTR.load(Ordering::Acquire);
            if canvas_view_ptr != 0 {
                let canvas_view = canvas_view_ptr as *mut AnyObject;
                let _: () = unsafe {
                    msg_send![
                        canvas_view,
                        performSelectorOnMainThread: sel!(renderTelemetry)
                        withObject: std::ptr::null_mut::<AnyObject>()
                        waitUntilDone: false
                    ]
                };
            }
        }
    });

    let mtm = MainThreadMarker::new().expect("Must run on main thread");
    let app = NSApplication::sharedApplication(mtm);
    let _changed = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let delegate = AppDelegate::new(consumer, mtm);
    let delegate_protocol: &ProtocolObject<dyn NSApplicationDelegate> =
        ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(delegate_protocol));

    unsafe { app.run() };
}

fn network_activity_percent(bytes_per_sec: u64) -> f32 {
    if bytes_per_sec == 0 {
        return 0.0;
    }

    (((bytes_per_sec as f32 + 1.0).log10() / 7.0) * 100.0).clamp(0.0, 100.0)
}

fn max_component_temperature(components: &sysinfo::Components) -> f32 {
    components
        .iter()
        .map(|component| component.temperature())
        .filter(|temperature| temperature.is_finite())
        .fold(0.0, f32::max)
}
