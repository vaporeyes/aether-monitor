// ABOUTME: Owns the headless wgpu surface and egui renderer for AppKit views.
// ABOUTME: Renders telemetry frames into a CAMetalLayer-backed surface.

use std::ffi::c_void;

use egui::{Context, RawInput};
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
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.heading("Aether Monitor");
        ui.separator();
        ui.label(format!("CPU {:>5.1}%", frame.cpu_total));
        ui.add(egui::ProgressBar::new(frame.cpu_total / 100.0).show_percentage());
        ui.label(format!(
            "Memory {} MB / {} MB",
            frame.mem_used_mb, frame.mem_total_mb
        ));
        ui.add(egui::ProgressBar::new(memory_ratio(frame)).show_percentage());
        ui.label(format!(
            "Network in {} B/s  out {} B/s",
            frame.net_in_bytes_sec, frame.net_out_bytes_sec
        ));
        ui.label(format!("Temperature {:>4.1} C", frame.temp_celsius));
    });
}

fn memory_ratio(frame: &TelemetryFrame) -> f32 {
    if frame.mem_total_mb == 0 {
        return 0.0;
    }

    (frame.mem_used_mb as f32 / frame.mem_total_mb as f32).clamp(0.0, 1.0)
}
