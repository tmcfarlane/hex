//! The homepage's glossy bean, drawn natively over a Rune overlay.

/// A character frame. Coordinates are device pixels in `output`.
#[derive(Clone, Copy)]
pub(crate) struct Draw {
    pub(crate) rect: [f32; 4],
    pub(crate) output: [u32; 2],
    pub(crate) time: f32,
    pub(crate) level: f32,
    pub(crate) reduced: bool,
}

impl Draw {
    fn rig(self) -> [f32; 4] {
        let mouth = if self.level.is_finite() {
            self.level.clamp(0.0, 1.0).sqrt() * 0.55
        } else {
            0.0
        };
        if self.reduced {
            return [mouth, 0.0, 0.0, 0.0];
        }
        let time = if self.time.is_finite() {
            self.time
        } else {
            0.0
        };
        let blink = (1.0 - ((time.rem_euclid(4.7) - 4.3) / 0.085).abs()).max(0.0);
        [
            mouth,
            blink,
            (time * 0.78).sin() * 0.025,
            (time * 1.25).sin() * 0.025,
        ]
    }

    /// Where words leave the mouth, using the shader's scale and rotation.
    pub(crate) fn mouth(self) -> [f32; 2] {
        let [x, y, width, height] = self.rect;
        let [mouth, _, tilt, bob] = self.rig();
        let radius = width.min(height) * 0.39;
        let sy = 1.0 + mouth * 0.05;
        let local_y = 0.31 * sy * radius;
        let (sin, cos) = tilt.sin_cos();
        [
            x + width * 0.5 - local_y * sin,
            y + height * 0.5 + bob * radius + local_y * cos,
        ]
    }

    fn uniforms(self) -> [f32; 16] {
        let [x, y, width, height] = self.rect;
        let [mouth, blink, tilt, bob] = self.rig();
        [
            self.output[0].max(1) as f32,
            self.output[1].max(1) as f32,
            0.0,
            0.0,
            x,
            y,
            width,
            height,
            mouth,
            blink,
            tilt,
            bob,
            0.2,
            0.15,
            0.0,
            0.0,
        ]
    }
}

pub(crate) struct Painter {
    pub(crate) format: wgpu::TextureFormat,
    uniforms: wgpu::Buffer,
    bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
}

impl Painter {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Hex buddy"),
            source: wgpu::ShaderSource::Wgsl(include_str!("hex_buddy.wgsl").into()),
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Hex buddy frame"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Hex buddy"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Hex buddy"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Hex buddy"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let constants = [("linear_target", f64::from(u8::from(format.is_srgb())))];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Hex buddy"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("buddy_vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("buddy_fragment"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            format,
            uniforms,
            bind,
            pipeline,
        }
    }

    pub(crate) fn write(&self, frame: Draw, mut write: impl FnMut(&wgpu::Buffer, &[u8])) {
        let bytes = frame.uniforms().map(f32::to_ne_bytes);
        write(&self.uniforms, bytes.as_flattened());
    }

    pub(crate) fn draw_over(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Hex buddy"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn buddy_shader_validates_without_a_gpu() {
        let source = include_str!("hex_buddy.wgsl");
        let module = wgpu::naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
        let info = wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
        for linear in [0.0, 1.0] {
            wgpu::naga::back::pipeline_constants::process_overrides(
                &module,
                &info,
                Some((wgpu::naga::ShaderStage::Fragment, "buddy_fragment")),
                &[("linear_target".into(), linear)].into_iter().collect(),
            )
            .expect("the character must compile in each target colour space");
        }
    }
}
