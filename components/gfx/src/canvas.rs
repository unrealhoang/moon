use super::backend::{Backend, DrawRequest};
use super::Bitmap;
use crate::painters::polygon::PolygonPainter;
use crate::painters::rect::RectPainter;
use crate::painters::text::TextPainter;
use crate::tessellator::Tessellator;
use crate::Graphics;
use async_trait::async_trait;
use futures::task::SpawnExt;
use shared::color::Color;
use shared::primitive::*;

pub struct Canvas<'a> {
    tessellator: Tessellator,
    polygon_painter: PolygonPainter,
    rect_painter: RectPainter,
    text_painter: TextPainter,
    backend: Backend,
    device: wgpu::Device,
    queue: wgpu::Queue,
    staging_belt: wgpu::util::StagingBelt,
    local_pool: futures::executor::LocalPool,
    frame_desc: wgpu::TextureDescriptor<'a>,
    frame: wgpu::Texture,
    frame_texture_view: wgpu::TextureView,
    output_buffer: wgpu::Buffer,
    output_buffer_desc: wgpu::BufferDescriptor<'a>,
}

pub const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl<'a> Canvas<'a> {
    const CHUNK_SIZE: u64 = 10 * 1024;

    pub async fn new() -> Canvas<'a> {
        let instance = wgpu::Instance::new(wgpu::Backends::PRIMARY);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .unwrap();

        let (device, queue) = adapter
            .request_device(&Default::default(), None)
            .await
            .unwrap();

        let staging_belt = wgpu::util::StagingBelt::new(Self::CHUNK_SIZE);
        let local_pool = futures::executor::LocalPool::new();

        let frame_desc = wgpu::TextureDescriptor {
            label: Some("moon output texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT,
        };

        let frame = device.create_texture(&frame_desc);

        let frame_texture_view = frame.create_view(&Default::default());
        let output_buffer_desc = wgpu::BufferDescriptor {
            label: Some("moon output buffer"),
            size: 1,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        };
        let output_buffer = device.create_buffer(&output_buffer_desc);

        Self {
            backend: Backend::new(&device, TEXTURE_FORMAT),
            tessellator: Tessellator::new(),
            polygon_painter: PolygonPainter::new(),
            rect_painter: RectPainter::new(),
            text_painter: TextPainter::new(),
            device,
            queue,
            staging_belt,
            local_pool,
            frame_desc,
            frame,
            frame_texture_view,
            output_buffer,
            output_buffer_desc,
        }
    }

    pub fn resize(&mut self, size: (u32, u32)) {
        let (width, height) = size;
        self.frame_desc.size.width = width;
        self.frame_desc.size.height = height;

        self.output_buffer_desc.size = (self.get_bytes_per_row() * height) as u64;

        self.frame = self.device.create_texture(&self.frame_desc);
        self.frame_texture_view = self.frame.create_view(&Default::default());
        self.output_buffer = self.device.create_buffer(&self.output_buffer_desc);
    }

    pub fn paint(&mut self) {
        let triangles = self.tessellator.vertex_buffers();
        let texts = self.text_painter.texts();

        let request = DrawRequest { triangles, texts };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("moon wgpu encoder"),
            });

        // Background clear
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("moon::gfx clear bg render pass"),
            color_attachments: &[wgpu::RenderPassColorAttachment {
                view: &self.frame_texture_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: true,
                },
            }],
            depth_stencil_attachment: None,
        });

        self.backend.draw(
            &self.device,
            &mut encoder,
            &mut self.staging_belt,
            &self.frame.create_view(&Default::default()),
            (self.frame_desc.size.width, self.frame_desc.size.height),
            request,
        );

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &self.frame,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.output_buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: core::num::NonZeroU32::new(self.get_bytes_per_row()),
                    rows_per_image: core::num::NonZeroU32::new(self.frame_desc.size.height),
                },
            },
            self.frame_desc.size,
        );

        self.staging_belt.finish();
        self.queue.submit(Some(encoder.finish()));
        self.local_pool
            .spawner()
            .spawn(self.staging_belt.recall())
            .expect("Recall staging belt");

        self.local_pool.run_until_stalled();

        // clean up for next draw
        self.text_painter.clear();
        self.tessellator.clear();
    }

    fn get_bytes_per_row(&self) -> u32 {
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let unpadded_bytes_per_row = 4 * self.frame_desc.size.width;
        let padding = alignment - unpadded_bytes_per_row % alignment;
        let bytes_per_row = padding + unpadded_bytes_per_row;

        bytes_per_row
    }

    pub async fn output(&mut self) -> Bitmap {
        let buffer_slice = self.output_buffer.slice(..);

        // NOTE: We have to create the mapping THEN device.poll() before await
        // the future. Otherwise the application will freeze.
        let mapping = buffer_slice.map_async(wgpu::MapMode::Read);
        self.device.poll(wgpu::Maintain::Wait);

        mapping.await.unwrap();

        let aligned_output = buffer_slice.get_mapped_range().to_vec();

        let mut output = Vec::new();
        let mut row_pointer: usize = 0;

        let unpadded_bytes_per_row = 4 * self.frame_desc.size.width;

        for _ in 0..self.frame_desc.size.height {
            let row = &aligned_output[row_pointer..row_pointer + unpadded_bytes_per_row as usize];
            output.extend_from_slice(row);
            row_pointer += self.get_bytes_per_row() as usize;
        }

        self.output_buffer.unmap();

        output
    }
}

#[async_trait(?Send)]
impl<'a> Graphics for Canvas<'a> {
    fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.rect_painter
            .draw_solid_rect(&mut self.tessellator, &rect, &color);
    }

    fn fill_rrect(&mut self, rect: RRect, color: Color) {
        self.rect_painter
            .draw_solid_rrect(&mut self.tessellator, &rect, &color);
    }

    fn fill_text(&mut self, content: String, bounds: Rect, color: Color, size: f32) {
        self.text_painter.fill_text(content, bounds, color, size);
    }

    fn fill_polygon(&mut self, points: Vec<Point>, color: Color) {
        self.polygon_painter
            .fill_polygon(&mut self.tessellator, &points, &color);
    }

    fn resize(&mut self, size: Size) {
        self.resize((size.width as u32, size.height as u32));
    }

    async fn output(&mut self) -> Vec<u8> {
        self.paint();
        self.output().await
    }
}
