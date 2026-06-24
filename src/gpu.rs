// ABOUTME: Owns the headless wgpu surface and egui renderer for AppKit views.
// ABOUTME: Renders telemetry frames into a CAMetalLayer-backed surface.

use std::ffi::c_void;

use egui::{Color32, Context, Frame, Margin, RawInput, Rounding, Stroke, Vec2};
use egui_wgpu::wgpu::{
    CommandEncoderDescriptor, Device, LoadOp, Operations, Queue, RenderPassColorAttachment,
    RenderPassDescriptor, StoreOp, Surface, SurfaceConfiguration, TextureViewDescriptor,
};
use egui_wgpu::{Renderer, ScreenDescriptor, wgpu};

use crate::telemetry::TelemetryFrame;

/// Failure modes when binding the headless wgpu surface to a CAMetalLayer.
#[derive(Debug)]
pub enum GpuInitError {
    SurfaceCreation(wgpu::CreateSurfaceError),
    NoAdapter,
    DeviceRequest(wgpu::RequestDeviceError),
}

impl std::fmt::Display for GpuInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuInitError::SurfaceCreation(error) => {
                write!(f, "failed to bind Metal surface: {error}")
            }
            GpuInitError::NoAdapter => write!(f, "no Metal adapter found"),
            GpuInitError::DeviceRequest(error) => {
                write!(f, "Metal device request failed: {error}")
            }
        }
    }
}

impl std::error::Error for GpuInitError {}

pub struct GpuEngine<'a> {
    pub egui_ctx: Context,
    device: Device,
    queue: Queue,
    surface: Surface<'a>,
    surface_config: SurfaceConfiguration,
    renderer: Renderer,
}

impl<'a> GpuEngine<'a> {
    /// Creates a renderer bound to an Objective-C `CAMetalLayer`.
    ///
    /// # Safety
    ///
    /// `metal_layer_ptr` must point to a valid live `CAMetalLayer`. The layer
    /// must outlive the returned engine because wgpu keeps using it as the
    /// surface target. The caller must also ensure this is created from the
    /// AppKit main thread that owns the view hierarchy.
    pub unsafe fn new_from_metal_layer(
        metal_layer_ptr: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<Self, GpuInitError> {
        let instance = wgpu::Instance::default();
        let target = wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(metal_layer_ptr);
        let surface = unsafe { instance.create_surface_unsafe(target) }
            .map_err(GpuInitError::SurfaceCreation)?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or(GpuInitError::NoAdapter)?;

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
                .map_err(GpuInitError::DeviceRequest)?;

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps.formats[0];
        let surface_config = SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::PostMultiplied,
            view_formats: vec![],
        };

        surface.configure(&device, &surface_config);

        let renderer = Renderer::new(&device, surface_format, None, 1);

        Ok(Self {
            egui_ctx: Context::default(),
            device,
            queue,
            surface,
            surface_config,
            renderer,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        // Reconfiguring the surface reallocates the swapchain; skip it unless the
        // dimensions actually changed to avoid per-frame churn and flicker.
        if self.surface_config.width == width && self.surface_config.height == height {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    pub fn render(
        &mut self,
        frame: &TelemetryFrame,
        events: Vec<egui::Event>,
    ) -> Result<(), wgpu::SurfaceError> {
        let raw_input = RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(
                    self.surface_config.width as f32,
                    self.surface_config.height as f32,
                ),
            )),
            events,
            ..RawInput::default()
        };

        let output = self.egui_ctx.run(raw_input, |ctx| {
            draw_monitor_panel(ctx, frame);
        });

        let surface_texture = self.surface.get_current_texture()?;
        let view = surface_texture
            .texture
            .create_view(&TextureViewDescriptor::default());
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point: output.pixels_per_point,
        };
        let paint_jobs = self
            .egui_ctx
            .tessellate(output.shapes, output.pixels_per_point);

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("aether_monitor_encoder"),
            });

        for (texture_id, image_delta) in &output.textures_delta.set {
            self.renderer
                .update_texture(&self.device, &self.queue, *texture_id, image_delta);
        }

        let command_buffers = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );

        {
            let mut render_pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("aether_monitor_render_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.025,
                            b: 0.03,
                            a: 0.92,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.renderer
                .render(&mut render_pass, &paint_jobs, &screen_descriptor);
        }

        for texture_id in &output.textures_delta.free {
            self.renderer.free_texture(texture_id);
        }

        self.queue.submit(command_buffers);
        self.queue.submit(Some(encoder.finish()));
        surface_texture.present();

        Ok(())
    }
}

fn draw_monitor_panel(ctx: &Context, frame: &TelemetryFrame) {
    let mut style = (*ctx.style()).clone();
    style.visuals.override_text_color = Some(Color32::from_rgb(224, 231, 238));
    style.visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(18, 21, 25);
    style.spacing.item_spacing = Vec2::new(6.0, 6.0);
    ctx.set_style(style);

    egui::CentralPanel::default()
        .frame(Frame::none().fill(Color32::from_rgb(10, 12, 15)))
        .show(ctx, |ui| {
            ui.set_width(ui.available_width());
            ui.add_space(8.0);
            Frame::none()
                .inner_margin(Margin::symmetric(10.0, 0.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("AETHER")
                            .size(9.0)
                            .color(Color32::from_rgb(122, 146, 164)),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("System Monitor")
                            .size(22.0)
                            .color(Color32::from_rgb(241, 246, 249)),
                    );
                });
            ui.add_space(8.0);

            metric_row(
                ui,
                "CPU",
                format!("{:>4.1}%", frame.cpu_total),
                frame.cpu_total / 100.0,
                Color32::from_rgb(92, 222, 255),
            );
            metric_row(
                ui,
                "Memory",
                format!(
                    "{} / {} GB",
                    mb_to_gb(frame.mem_used_mb),
                    mb_to_gb(frame.mem_total_mb)
                ),
                memory_ratio(frame),
                Color32::from_rgb(112, 158, 255),
            );
            metric_row(
                ui,
                "Network",
                format!(
                    "{} in  {} out",
                    compact_bytes(frame.net_in_bytes_sec),
                    compact_bytes(frame.net_out_bytes_sec),
                ),
                latest_network_ratio(frame),
                Color32::from_rgb(36, 232, 160),
            );
            metric_row(
                ui,
                "Thermal",
                format!("{:>4.1} C", frame.temp_celsius),
                frame.temp_celsius / 100.0,
                thermal_color(frame.temp_celsius),
            );
        });
}

fn metric_row(ui: &mut egui::Ui, label: &str, value: String, ratio: f32, accent: Color32) {
    Frame::none()
        .fill(Color32::from_rgb(15, 18, 22))
        .stroke(Stroke::new(1.0, Color32::from_rgb(37, 43, 50)))
        .rounding(Rounding::same(4.0))
        .inner_margin(Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_height(18.0);
                ui.add_sized(
                    Vec2::new(48.0, 18.0),
                    egui::Label::new(
                        egui::RichText::new(label)
                            .size(10.0)
                            .color(Color32::from_rgb(130, 148, 160)),
                    )
                    .wrap(false),
                );
                ui.add_sized(
                    Vec2::new(168.0, 18.0),
                    egui::Label::new(
                        egui::RichText::new(value)
                            .size(15.0)
                            .color(Color32::from_rgb(238, 244, 248)),
                    )
                    .wrap(false),
                );
                ui.add_space(4.0);
                ui.add(
                    egui::ProgressBar::new(ratio.clamp(0.0, 1.0))
                        .fill(accent)
                        .desired_width((ui.available_width() - 2.0).max(60.0))
                        .desired_height(5.0),
                );
            });
        });
}

fn memory_ratio(frame: &TelemetryFrame) -> f32 {
    if frame.mem_total_mb == 0 {
        return 0.0;
    }

    (frame.mem_used_mb as f32 / frame.mem_total_mb as f32).clamp(0.0, 1.0)
}

fn latest_network_ratio(frame: &TelemetryFrame) -> f32 {
    frame.net_activity_history[59] / 100.0
}

fn thermal_color(temperature: f32) -> Color32 {
    if temperature >= 80.0 {
        Color32::from_rgb(255, 72, 56)
    } else if temperature >= 60.0 {
        Color32::from_rgb(255, 174, 54)
    } else {
        Color32::from_rgb(181, 238, 90)
    }
}

fn compact_bytes(bytes_per_sec: u64) -> String {
    if bytes_per_sec >= 1_000_000 {
        return format!("{:.1} MB/s", bytes_per_sec as f32 / 1_000_000.0);
    }
    if bytes_per_sec >= 1_000 {
        return format!("{:.1} KB/s", bytes_per_sec as f32 / 1_000.0);
    }

    format!("{bytes_per_sec} B/s")
}

fn mb_to_gb(mebibytes: u64) -> String {
    format!("{:.1}", mebibytes as f32 / 1024.0)
}
